// Resonance Audio Processing Object — thin C++ COM shell.
//
// The Windows audio engine instantiates system-effect APOs via COM aggregation,
// which the SDK base class CBaseAudioProcessingObject handles. This shell does
// only the COM/aggregation/lifecycle boilerplate and forwards the actual DSP to
// the Rust engine (resonance_apo.lib, the resonance-apo crate) over a C ABI.
//
// Modeled on EqualizerAPO's COM layer (GPL), reduced to a single effect with no
// child-APO chaining.

#define INITGUID
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <unknwn.h>
#include <audioenginebaseapo.h>
#include <baseaudioprocessingobject.h>
#include <audiomediatype.h>
#include <mmreg.h>
#include <new>
#include <cstdint>
#include <cstring>
#include <cstdarg>

// {7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}
DEFINE_GUID(CLSID_ResonanceAPO, 0x7c3d2a1e, 0x9b6f, 0x4e2a, 0x8d, 0x5c, 0x1f, 0x0a,
            0x3b, 0x4c, 0x5d, 0x6e);

// ---- Rust engine C ABI (resonance-apo crate) ----
extern "C" {
void* resonance_apo_create();
void resonance_apo_lock(void* p, uint32_t channels, double sample_rate, uint32_t max_frames);
void resonance_apo_unlock(void* p);
void resonance_apo_process(void* p, float* buf, uint32_t frames, uint32_t channels);
void resonance_apo_destroy(void* p);
void resonance_apo_log(const char* msg);
}

#include <cstdio>
static void rlog(const char* fmt, ...) {
    char buf[256];
    va_list ap;
    va_start(ap, fmt);
    _vsnprintf_s(buf, sizeof(buf), _TRUNCATE, fmt, ap);
    va_end(ap);
    resonance_apo_log(buf);
}

static HINSTANCE g_hModule = nullptr;
static long g_instCount = 0;
static long g_lockCount = 0;

// Non-delegating IUnknown, exposed by an aggregated inner object.
struct INonDelegatingUnknown {
    virtual HRESULT __stdcall NonDelegatingQueryInterface(REFIID iid, void** ppv) = 0;
    virtual ULONG __stdcall NonDelegatingAddRef() = 0;
    virtual ULONG __stdcall NonDelegatingRelease() = 0;
};

class ResonanceAPO : public CBaseAudioProcessingObject,
                     public IAudioSystemEffects,
                     public INonDelegatingUnknown {
public:
    explicit ResonanceAPO(IUnknown* pUnkOuter);
    virtual ~ResonanceAPO();

    // IUnknown (delegating — forwards to the controlling outer)
    HRESULT __stdcall QueryInterface(REFIID iid, void** ppv) override;
    ULONG __stdcall AddRef() override;
    ULONG __stdcall Release() override;

    // IAudioProcessingObject
    HRESULT __stdcall GetLatency(HNSTIME* pTime) override;
    HRESULT __stdcall Initialize(UINT32 cbDataSize, BYTE* pbyData) override;

    // IAudioProcessingObjectConfiguration
    HRESULT __stdcall LockForProcess(UINT32 numInput, APO_CONNECTION_DESCRIPTOR** ppInput,
                                     UINT32 numOutput,
                                     APO_CONNECTION_DESCRIPTOR** ppOutput) override;
    HRESULT __stdcall UnlockForProcess() override;

    // IAudioProcessingObjectRT
    void __stdcall APOProcess(UINT32 numInput, APO_CONNECTION_PROPERTY** ppInput,
                              UINT32 numOutput, APO_CONNECTION_PROPERTY** ppOutput) override;

    // INonDelegatingUnknown
    HRESULT __stdcall NonDelegatingQueryInterface(REFIID iid, void** ppv) override;
    ULONG __stdcall NonDelegatingAddRef() override;
    ULONG __stdcall NonDelegatingRelease() override;

    static const CRegAPOProperties<1> regProperties;

private:
    long m_refCount;
    IUnknown* m_pUnkOuter;
    void* m_engine;   // Rust ApoEngine*
    UINT32 m_channels;
};

const CRegAPOProperties<1> ResonanceAPO::regProperties(
    CLSID_ResonanceAPO, L"Resonance APO", L"Resonance", 1, 0,
    __uuidof(IAudioProcessingObject),
    (APO_FLAG)(APO_FLAG_FRAMESPERSECOND_MUST_MATCH | APO_FLAG_SAMPLESPERFRAME_MUST_MATCH |
               APO_FLAG_BITSPERSAMPLE_MUST_MATCH | APO_FLAG_INPLACE));

ResonanceAPO::ResonanceAPO(IUnknown* pUnkOuter)
    : CBaseAudioProcessingObject(regProperties), m_refCount(1), m_engine(nullptr), m_channels(2) {
    m_pUnkOuter = pUnkOuter ? pUnkOuter
                            : reinterpret_cast<IUnknown*>(static_cast<INonDelegatingUnknown*>(this));
    m_engine = resonance_apo_create();
    InterlockedIncrement(&g_instCount);
}

ResonanceAPO::~ResonanceAPO() {
    if (m_engine) resonance_apo_destroy(m_engine);
    InterlockedDecrement(&g_instCount);
}

// Delegating IUnknown
HRESULT __stdcall ResonanceAPO::QueryInterface(REFIID iid, void** ppv) {
    return m_pUnkOuter->QueryInterface(iid, ppv);
}
ULONG __stdcall ResonanceAPO::AddRef() { return m_pUnkOuter->AddRef(); }
ULONG __stdcall ResonanceAPO::Release() { return m_pUnkOuter->Release(); }

// Non-delegating IUnknown
HRESULT __stdcall ResonanceAPO::NonDelegatingQueryInterface(REFIID iid, void** ppv) {
    if (iid == __uuidof(IUnknown))
        *ppv = static_cast<INonDelegatingUnknown*>(this);
    else if (iid == __uuidof(IAudioProcessingObject))
        *ppv = static_cast<IAudioProcessingObject*>(this);
    else if (iid == __uuidof(IAudioProcessingObjectRT))
        *ppv = static_cast<IAudioProcessingObjectRT*>(this);
    else if (iid == __uuidof(IAudioProcessingObjectConfiguration))
        *ppv = static_cast<IAudioProcessingObjectConfiguration*>(this);
    else if (iid == __uuidof(IAudioSystemEffects))
        *ppv = static_cast<IAudioSystemEffects*>(this);
    else {
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    reinterpret_cast<IUnknown*>(*ppv)->AddRef();
    return S_OK;
}
ULONG __stdcall ResonanceAPO::NonDelegatingAddRef() { return InterlockedIncrement(&m_refCount); }
ULONG __stdcall ResonanceAPO::NonDelegatingRelease() {
    if (InterlockedDecrement(&m_refCount) == 0) {
        delete this;
        return 0;
    }
    return m_refCount;
}

HRESULT __stdcall ResonanceAPO::GetLatency(HNSTIME* pTime) {
    if (!pTime) return E_POINTER;
    *pTime = 0;
    return S_OK;
}

HRESULT __stdcall ResonanceAPO::Initialize(UINT32 cbDataSize, BYTE* pbyData) {
    rlog("cpp: Initialize cbDataSize=%u", cbDataSize);
    if (pbyData == nullptr && cbDataSize != 0) return E_INVALIDARG;
    if (pbyData != nullptr && cbDataSize == 0) return E_POINTER;
    // We don't need the APOInitSystemEffects payload; the daemon supplies the
    // DSP parameters over the shared-memory bridge.
    return S_OK;
}

HRESULT __stdcall ResonanceAPO::LockForProcess(UINT32 numInput,
                                               APO_CONNECTION_DESCRIPTOR** ppInput, UINT32 numOutput,
                                               APO_CONNECTION_DESCRIPTOR** ppOutput) {
    UNCOMPRESSEDAUDIOFORMAT inFormat;
    HRESULT hr = ppInput[0]->pFormat->GetUncompressedAudioFormat(&inFormat);
    if (FAILED(hr)) {
        rlog("cpp: LockForProcess GetUncompressedAudioFormat FAILED hr=0x%08X", hr);
        return hr;
    }
    UINT32 maxFrames = ppInput[0]->u32MaxFrameCount;

    hr = CBaseAudioProcessingObject::LockForProcess(numInput, ppInput, numOutput, ppOutput);
    rlog("cpp: base LockForProcess hr=0x%08X ch=%u rate=%.0f maxFrames=%u", hr,
         inFormat.dwSamplesPerFrame, inFormat.fFramesPerSecond, maxFrames);
    if (FAILED(hr)) return hr;

    m_channels = inFormat.dwSamplesPerFrame;
    if (m_engine)
        resonance_apo_lock(m_engine, m_channels, inFormat.fFramesPerSecond, maxFrames);
    return hr;
}

HRESULT __stdcall ResonanceAPO::UnlockForProcess() {
    if (m_engine) resonance_apo_unlock(m_engine);
    return CBaseAudioProcessingObject::UnlockForProcess();
}

void __stdcall ResonanceAPO::APOProcess(UINT32 numInput, APO_CONNECTION_PROPERTY** ppInput,
                                        UINT32 numOutput, APO_CONNECTION_PROPERTY** ppOutput) {
    if (numInput == 0 || numOutput == 0) return;
    APO_CONNECTION_PROPERTY* in = ppInput[0];
    APO_CONNECTION_PROPERTY* out = ppOutput[0];

    static bool logged = false;
    if (!logged) {
        logged = true;
        rlog("cpp: APOProcess first call flags=%u inplace=%d frames=%u ch=%u",
             in->u32BufferFlags, (in->pBuffer == out->pBuffer) ? 1 : 0, in->u32ValidFrameCount,
             m_channels);
    }

    switch (in->u32BufferFlags) {
    case BUFFER_VALID: {
        float* inF = reinterpret_cast<float*>(in->pBuffer);
        float* outF = reinterpret_cast<float*>(out->pBuffer);
        UINT32 frames = in->u32ValidFrameCount;
        // Process into the OUTPUT buffer. With APO_FLAG_INPLACE the engine
        // normally gives out == in, but don't rely on it: copy first if they
        // differ, otherwise the output would be left unwritten (silence).
        if (outF != inF)
            memcpy(outF, inF, static_cast<size_t>(frames) * m_channels * sizeof(float));
        resonance_apo_process(m_engine, outF, frames, m_channels);
        out->u32ValidFrameCount = frames;
        out->u32BufferFlags = BUFFER_VALID;
        break;
    }
    case BUFFER_SILENT:
        out->u32ValidFrameCount = in->u32ValidFrameCount;
        out->u32BufferFlags = BUFFER_SILENT;
        break;
    default:
        break;
    }
}

// ---- Class factory (handles aggregation by passing the outer to the APO) ----
class ClassFactory : public IClassFactory {
public:
    ClassFactory() : m_refCount(1) {}
    HRESULT __stdcall QueryInterface(REFIID iid, void** ppv) override {
        if (iid == __uuidof(IUnknown) || iid == __uuidof(IClassFactory))
            *ppv = static_cast<IClassFactory*>(this);
        else {
            *ppv = nullptr;
            return E_NOINTERFACE;
        }
        reinterpret_cast<IUnknown*>(*ppv)->AddRef();
        return S_OK;
    }
    ULONG __stdcall AddRef() override { return InterlockedIncrement(&m_refCount); }
    ULONG __stdcall Release() override {
        if (InterlockedDecrement(&m_refCount) == 0) {
            delete this;
            return 0;
        }
        return m_refCount;
    }
    HRESULT __stdcall CreateInstance(IUnknown* pUnkOuter, REFIID iid, void** ppv) override {
        if (pUnkOuter != nullptr && iid != __uuidof(IUnknown)) return E_NOINTERFACE;
        ResonanceAPO* apo = new (std::nothrow) ResonanceAPO(pUnkOuter);
        if (apo == nullptr) return E_OUTOFMEMORY;
        HRESULT hr = apo->NonDelegatingQueryInterface(iid, ppv);
        apo->NonDelegatingRelease();
        return hr;
    }
    HRESULT __stdcall LockServer(BOOL bLock) override {
        if (bLock) InterlockedIncrement(&g_lockCount);
        else InterlockedDecrement(&g_lockCount);
        return S_OK;
    }

private:
    long m_refCount;
};

// ---- DLL exports ----
BOOL WINAPI DllMain(HINSTANCE hModule, DWORD reason, void*) {
    if (reason == DLL_PROCESS_ATTACH) g_hModule = hModule;
    return TRUE;
}

STDAPI DllCanUnloadNow() {
    return (g_instCount == 0 && g_lockCount == 0) ? S_OK : S_FALSE;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** ppv) {
    if (clsid != CLSID_ResonanceAPO) return CLASS_E_CLASSNOTAVAILABLE;
    ClassFactory* factory = new (std::nothrow) ClassFactory();
    if (factory == nullptr) return E_OUTOFMEMORY;
    HRESULT hr = factory->QueryInterface(iid, ppv);
    factory->Release();
    return hr;
}

STDAPI DllRegisterServer() {
    HRESULT hr = RegisterAPO(ResonanceAPO::regProperties);
    if (FAILED(hr)) return hr;

    wchar_t path[MAX_PATH];
    GetModuleFileNameW(g_hModule, path, MAX_PATH);

    // InprocServer32 for the COM class.
    HKEY key;
    const wchar_t* clsidKey =
        L"SOFTWARE\\Classes\\CLSID\\{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}\\InprocServer32";
    if (RegCreateKeyExW(HKEY_LOCAL_MACHINE, clsidKey, 0, nullptr, 0, KEY_WRITE, nullptr, &key,
                        nullptr) == ERROR_SUCCESS) {
        RegSetValueExW(key, nullptr, 0, REG_SZ, (const BYTE*)path,
                       (DWORD)((wcslen(path) + 1) * sizeof(wchar_t)));
        const wchar_t* both = L"Both";
        RegSetValueExW(key, L"ThreadingModel", 0, REG_SZ, (const BYTE*)both,
                       (DWORD)((wcslen(both) + 1) * sizeof(wchar_t)));
        RegCloseKey(key);
    }
    return S_OK;
}

STDAPI DllUnregisterServer() {
    UnregisterAPO(CLSID_ResonanceAPO);
    RegDeleteTreeW(HKEY_LOCAL_MACHINE,
                   L"SOFTWARE\\Classes\\CLSID\\{7C3D2A1E-9B6F-4E2A-8D5C-1F0A3B4C5D6E}");
    return S_OK;
}
