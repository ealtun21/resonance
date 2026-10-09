// tapcap: creates a Process Tap plus an aggregate device with options taken from the
// environment, records every input channel of the aggregate for SECS seconds into OUT
// (interleaved f32le) and prints the device properties it ran with. Driven by the
// tap_leg example in crates/resonance-e2e; build and sign it as described in
// contrib/e2e/macos/README.md.
// env: TAP=dev|global|mono  DEVUID=<uid>  DRIFT=0|1  QUALITY=n  SUB=<uid> SUBDRIFT=0|1 MASTER=1 CLOCK=<uid>
//      RATE=<hz nominal on aggregate>  SECS=n  OUT=<file>  STACKED=0|1  MUTE=0|1|2  MASTERTAP=1
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardware.h>
#import <CoreAudio/CATapDescription.h>
#import <CoreAudio/AudioHardwareTapping.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static FILE *gout;
static volatile unsigned long gcalls, gnz;
static UInt32 gtotalch;

static OSStatus ioproc(AudioObjectID dev, const AudioTimeStamp *now, const AudioBufferList *in,
                       const AudioTimeStamp *intime, AudioBufferList *out, const AudioTimeStamp *outtime, void *ctx) {
  gcalls++;
  if (!in || in->mNumberBuffers == 0) return 0;
  UInt32 frames = in->mBuffers[0].mDataByteSize / (in->mBuffers[0].mNumberChannels * 4);
  UInt32 tot = 0;
  for (UInt32 b = 0; b < in->mNumberBuffers; b++) tot += in->mBuffers[b].mNumberChannels;
  gtotalch = tot;
  for (UInt32 f = 0; f < frames; f++) {
    for (UInt32 b = 0; b < in->mNumberBuffers; b++) {
      const float *p = in->mBuffers[b].mData;
      UInt32 nc = in->mBuffers[b].mNumberChannels;
      fwrite(p + f * nc, 4, nc, gout);
      for (UInt32 c = 0; c < nc; c++) if (p[f * nc + c] != 0.0f) gnz++;
    }
  }
  return 0;
}

static void dumpfmt(const char *tag, AudioObjectID o, AudioObjectPropertySelector sel, AudioObjectPropertyScope sc) {
  AudioObjectPropertyAddress a = {sel, sc, kAudioObjectPropertyElementMain};
  AudioStreamBasicDescription d; UInt32 sz = sizeof d;
  OSStatus s = AudioObjectGetPropertyData(o, &a, 0, NULL, &sz, &d);
  if (s) { printf("%s: err %d\n", tag, (int)s); return; }
  printf("%s: %.1f Hz, %u ch, bpf %u, flags 0x%x\n", tag, d.mSampleRate, d.mChannelsPerFrame, d.mBytesPerFrame, d.mFormatFlags);
}

static void dumpstreams(AudioObjectID dev) {
  for (int k = 0; k < 2; k++) {
    AudioObjectPropertyScope sc = k ? kAudioObjectPropertyScopeOutput : kAudioObjectPropertyScopeInput;
    AudioObjectPropertyAddress a = {kAudioDevicePropertyStreams, sc, kAudioObjectPropertyElementMain};
    UInt32 sz = 0; AudioObjectGetPropertyDataSize(dev, &a, 0, NULL, &sz);
    int n = sz / sizeof(AudioObjectID); AudioObjectID ids[32];
    if (n > 32) n = 32; sz = n * sizeof(AudioObjectID);
    AudioObjectGetPropertyData(dev, &a, 0, NULL, &sz, ids);
    for (int i = 0; i < n; i++) {
      char t[64]; snprintf(t, sizeof t, " %s stream %d virtual", k ? "out" : "in", i);
      dumpfmt(t, ids[i], kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal);
      snprintf(t, sizeof t, " %s stream %d physical", k ? "out" : "in", i);
      dumpfmt(t, ids[i], kAudioStreamPropertyPhysicalFormat, kAudioObjectPropertyScopeGlobal);
    }
  }
}

static CFStringRef cfs(const char *s) { return CFStringCreateWithCString(NULL, s, kCFStringEncodingUTF8); }

static AudioObjectID translate_pid(pid_t pid) {
  AudioObjectPropertyAddress a = {kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
  AudioObjectID o = 0; UInt32 sz = sizeof o;
  AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, sizeof pid, &pid, &sz, &o);
  return o;
}

int main(void) {
  @autoreleasepool {
    const char *mode = getenv("TAP") ?: "dev";
    const char *devuid = getenv("DEVUID") ?: "BlackHole2ch_UID";
    int drift = getenv("DRIFT") ? atoi(getenv("DRIFT")) : 1;
    int secs = getenv("SECS") ? atoi(getenv("SECS")) : 6;
    const char *outp = getenv("OUT") ?: "/tmp/tapcap.raw";
    NSNumber *me = [NSNumber numberWithUnsignedInt:translate_pid(getpid())];
    CATapDescription *desc;
    if (!strcmp(mode, "dev")) {
      desc = [[CATapDescription alloc] initExcludingProcesses:@[me] andDeviceUID:@(devuid) withStream:0];
    } else if (!strcmp(mode, "mono")) {
      desc = [[CATapDescription alloc] initMonoGlobalTapButExcludeProcesses:@[me]];
    } else {
      desc = [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:@[me]];
    }
    desc.name = @"tapcap";
    desc.privateTap = YES;
    desc.muteBehavior = getenv("MUTE") ? (CATapMuteBehavior)atoi(getenv("MUTE")) : CATapMutedWhenTapped;
    AudioObjectID tap = 0;
    OSStatus st = AudioHardwareCreateProcessTap(desc, &tap);
    printf("create tap: %d id %u\n", (int)st, tap);
    if (st) return 1;
    dumpfmt("tap format", tap, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal);
    AudioObjectPropertyAddress ua = {kAudioTapPropertyUID, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CFStringRef tapuid = NULL; UInt32 sz = sizeof tapuid;
    AudioObjectGetPropertyData(tap, &ua, 0, NULL, &sz, &tapuid);

    NSMutableDictionary *sub = [NSMutableDictionary dictionary];
    sub[@kAudioSubTapUIDKey] = (__bridge NSString *)tapuid;
    sub[@kAudioSubTapDriftCompensationKey] = @(drift != 0);
    if (getenv("QUALITY")) sub[@kAudioSubTapDriftCompensationQualityKey] = @(atoi(getenv("QUALITY")));
    NSMutableDictionary *agg = [@{
      @kAudioAggregateDeviceNameKey: @"tapcap-agg",
      @kAudioAggregateDeviceUIDKey: [NSString stringWithFormat:@"tapcap.%d", getpid()],
      @kAudioAggregateDeviceIsPrivateKey: @YES,
      @kAudioAggregateDeviceIsStackedKey: getenv("STACKED") ? @(atoi(getenv("STACKED"))) : @NO,
      @kAudioAggregateDeviceTapListKey: @[sub],
      @kAudioAggregateDeviceTapAutoStartKey: @NO,
    } mutableCopy];
    if (getenv("SUB")) {
      NSMutableDictionary *sd = [@{@kAudioSubDeviceUIDKey: @(getenv("SUB"))} mutableCopy];
      sd[@kAudioSubDeviceDriftCompensationKey] = @(getenv("SUBDRIFT") ? atoi(getenv("SUBDRIFT")) : 0);
      agg[@kAudioAggregateDeviceSubDeviceListKey] = @[sd];
      if (getenv("MASTER")) agg[@kAudioAggregateDeviceMainSubDeviceKey] = @(getenv("SUB"));
    }
    if (getenv("CLOCK")) agg[@kAudioAggregateDeviceClockDeviceKey] = strcmp(getenv("CLOCK"), "TAP") ? @(getenv("CLOCK")) : (__bridge NSString *)tapuid;
    if (getenv("MASTERTAP")) agg[@kAudioAggregateDeviceMainSubDeviceKey] = (__bridge NSString *)tapuid;
    AudioObjectID dev = 0;
    st = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)agg, &dev);
    printf("create aggregate: %d id %u\n", (int)st, dev);
    if (st) return 1;
    {
      AudioObjectPropertyAddress ca = {kAudioAggregateDevicePropertyComposition, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
      CFDictionaryRef comp = NULL; sz = sizeof comp;
      st = AudioObjectGetPropertyData(dev, &ca, 0, NULL, &sz, &comp);
      if (!st && comp) { NSLog(@"composition: %@", (__bridge NSDictionary *)comp); }
    }
    if (getenv("RATE")) {
      Float64 r = atof(getenv("RATE"));
      AudioObjectPropertyAddress ra = {kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
      st = AudioObjectSetPropertyData(dev, &ra, 0, NULL, sizeof r, &r);
      printf("set rate %.0f: %d\n", r, (int)st);
      usleep(500000);
    }
    {
      AudioObjectPropertyAddress ra = {kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
      Float64 r = 0; sz = sizeof r; AudioObjectGetPropertyData(dev, &ra, 0, NULL, &sz, &r);
      printf("aggregate nominal rate %.1f\n", r);
      AudioObjectPropertyAddress ca = {kAudioDevicePropertyClockDomain, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
      UInt32 cd = 0; sz = sizeof cd; st = AudioObjectGetPropertyData(dev, &ca, 0, NULL, &sz, &cd);
      printf("clock domain %u (%d)\n", cd, (int)st);
      AudioObjectPropertyAddress la = {kAudioDevicePropertyLatency, kAudioObjectPropertyScopeInput, kAudioObjectPropertyElementMain};
      UInt32 lat = 0; sz = sizeof lat; AudioObjectGetPropertyData(dev, &la, 0, NULL, &sz, &lat);
      printf("input latency %u frames\n", lat);
      AudioObjectPropertyAddress sa = {kAudioDevicePropertySafetyOffset, kAudioObjectPropertyScopeInput, kAudioObjectPropertyElementMain};
      UInt32 so = 0; sz = sizeof so; AudioObjectGetPropertyData(dev, &sa, 0, NULL, &sz, &so);
      printf("input safety offset %u frames\n", so);
    }
    dumpstreams(dev);
    gout = fopen(outp, "wb");
    AudioDeviceIOProcID pid = NULL;
    st = AudioDeviceCreateIOProcID(dev, ioproc, NULL, &pid);
    printf("ioproc: %d\n", (int)st);
    st = AudioDeviceStart(dev, pid);
    printf("start: %d\n", (int)st);
    fflush(stdout);
    sleep(secs);
    AudioDeviceStop(dev, pid);
    AudioDeviceDestroyIOProcID(dev, pid);
    fclose(gout);
    printf("calls %lu nonzero samples %lu channels %u\n", gcalls, gnz, gtotalch);
    AudioHardwareDestroyAggregateDevice(dev);
    AudioHardwareDestroyProcessTap(tap);
  }
  return 0;
}
