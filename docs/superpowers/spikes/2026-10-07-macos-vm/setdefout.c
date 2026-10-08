// Spike tool: list output devices, or set the default output by exact name.
// cc setdefout.c -framework CoreAudio -framework CoreFoundation -o setdefout
#include <CoreAudio/CoreAudio.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void name_of(AudioObjectID d, char *out, size_t n) {
    CFStringRef s = NULL;
    UInt32 sz = sizeof s;
    AudioObjectPropertyAddress a = {kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    out[0] = 0;
    if (AudioObjectGetPropertyData(d, &a, 0, NULL, &sz, &s) == 0 && s) {
        CFStringGetCString(s, out, n, kCFStringEncodingUTF8);
        CFRelease(s);
    }
}

static int has_output(AudioObjectID d) {
    AudioObjectPropertyAddress a = {kAudioDevicePropertyStreams, kAudioDevicePropertyScopeOutput, kAudioObjectPropertyElementMain};
    UInt32 sz = 0;
    return AudioObjectGetPropertyDataSize(d, &a, 0, NULL, &sz) == 0 && sz > 0;
}

int main(int argc, char **argv) {
    AudioObjectPropertyAddress la = {kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 sz = 0;
    AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &la, 0, NULL, &sz);
    AudioObjectID *ids = malloc(sz);
    AudioObjectGetPropertyData(kAudioObjectSystemObject, &la, 0, NULL, &sz, ids);
    int n = sz / sizeof(AudioObjectID);
    for (int i = 0; i < n; i++) {
        if (!has_output(ids[i])) continue;
        char nm[256];
        name_of(ids[i], nm, sizeof nm);
        if (argc < 2) printf("%u %s\n", ids[i], nm);
        else if (!strcmp(nm, argv[1])) {
            AudioObjectPropertyAddress da = {kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
            AudioObjectID d = ids[i];
            OSStatus st = AudioObjectSetPropertyData(kAudioObjectSystemObject, &da, 0, NULL, sizeof d, &d);
            printf("set default output to %s: %d\n", nm, (int)st);
            return st ? 1 : 0;
        }
    }
    if (argc >= 2) { fprintf(stderr, "no output device named %s\n", argv[1]); return 2; }
    return 0;
}
