// CoreAudio device control for the e2e guest (the guest has no brew / SwitchAudioSource).
//   audiodev list
//   audiodev default <name>        default output device (system output too)
//   audiodev rate <name> <hz>      nominal sample rate, polled until it reads back
// cc audiodev.c -framework CoreAudio -framework CoreFoundation -o audiodev
#include <CoreAudio/CoreAudio.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static AudioObjectPropertyAddress addr(AudioObjectPropertySelector s, AudioObjectPropertyScope sc) {
    AudioObjectPropertyAddress a = {s, sc, kAudioObjectPropertyElementMain};
    return a;
}

static void name_of(AudioObjectID d, char *out, size_t n) {
    CFStringRef s = NULL;
    UInt32 sz = sizeof s;
    AudioObjectPropertyAddress a = addr(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);
    out[0] = 0;
    if (AudioObjectGetPropertyData(d, &a, 0, NULL, &sz, &s) == 0 && s) {
        CFStringGetCString(s, out, n, kCFStringEncodingUTF8);
        CFRelease(s);
    }
}

static int channels(AudioObjectID d, AudioObjectPropertyScope scope) {
    AudioObjectPropertyAddress a = addr(kAudioDevicePropertyStreamConfiguration, scope);
    UInt32 sz = 0;
    if (AudioObjectGetPropertyDataSize(d, &a, 0, NULL, &sz) || !sz) return 0;
    AudioBufferList *l = malloc(sz);
    int n = 0;
    if (AudioObjectGetPropertyData(d, &a, 0, NULL, &sz, l) == 0)
        for (UInt32 i = 0; i < l->mNumberBuffers; i++) n += l->mBuffers[i].mNumberChannels;
    free(l);
    return n;
}

static Float64 rate_of(AudioObjectID d) {
    Float64 r = 0;
    UInt32 sz = sizeof r;
    AudioObjectPropertyAddress a = addr(kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal);
    AudioObjectGetPropertyData(d, &a, 0, NULL, &sz, &r);
    return r;
}

static AudioObjectID *devices(int *n) {
    AudioObjectPropertyAddress la = addr(kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal);
    UInt32 sz = 0;
    AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &la, 0, NULL, &sz);
    AudioObjectID *ids = malloc(sz);
    AudioObjectGetPropertyData(kAudioObjectSystemObject, &la, 0, NULL, &sz, ids);
    *n = sz / sizeof(AudioObjectID);
    return ids;
}

static AudioObjectID find(const char *name) {
    int n;
    AudioObjectID *ids = devices(&n), found = 0;
    for (int i = 0; i < n; i++) {
        char nm[256];
        name_of(ids[i], nm, sizeof nm);
        if (!strcmp(nm, name)) found = ids[i];
    }
    free(ids);
    return found;
}

int main(int argc, char **argv) {
    if (argc >= 2 && !strcmp(argv[1], "list")) {
        int n;
        AudioObjectID *ids = devices(&n);
        for (int i = 0; i < n; i++) {
            char nm[256];
            name_of(ids[i], nm, sizeof nm);
            printf("%u\t%s\tin=%d\tout=%d\t%.0f\n", ids[i], nm, channels(ids[i], kAudioDevicePropertyScopeInput),
                   channels(ids[i], kAudioDevicePropertyScopeOutput), rate_of(ids[i]));
        }
        return 0;
    }
    if (argc >= 3 && !strcmp(argv[1], "default")) {
        AudioObjectID d = find(argv[2]);
        if (!d) { fprintf(stderr, "no device named %s\n", argv[2]); return 2; }
        int st = 0;
        AudioObjectPropertySelector sels[] = {kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDefaultSystemOutputDevice};
        for (int i = 0; i < 2; i++) {
            AudioObjectPropertyAddress a = addr(sels[i], kAudioObjectPropertyScopeGlobal);
            st |= AudioObjectSetPropertyData(kAudioObjectSystemObject, &a, 0, NULL, sizeof d, &d);
        }
        return st ? 1 : 0;
    }
    if (argc >= 4 && !strcmp(argv[1], "rate")) {
        AudioObjectID d = find(argv[2]);
        if (!d) { fprintf(stderr, "no device named %s\n", argv[2]); return 2; }
        Float64 want = atof(argv[3]);
        AudioObjectPropertyAddress a = addr(kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal);
        if (AudioObjectSetPropertyData(d, &a, 0, NULL, sizeof want, &want)) return 1;
        for (int i = 0; i < 50; i++) {
            if (rate_of(d) == want) return 0;
            usleep(100000);
        }
        fprintf(stderr, "rate stayed %.0f\n", rate_of(d));
        return 1;
    }
    fprintf(stderr, "usage: audiodev list | default <name> | rate <name> <hz>\n");
    return 64;
}
