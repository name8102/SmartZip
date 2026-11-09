// Linux research probe, not a production adapter. Build against 7-Zip 26.06.
// Arguments: library archive [password|-] [test|read|cancel]
#include "CPP/Common/Common.h"
#include "CPP/Common/MyWindows.h"
#include "CPP/Common/MyInitGuid.h"
#include "CPP/Common/MyCom.h"
#include "CPP/7zip/Archive/IArchive.h"
#include "CPP/7zip/ICoder.h"
#include "CPP/7zip/IPassword.h"

#include <cstdio>
#include <clocale>
#include <cstring>
#include <cwchar>
#include <dlfcn.h>
#include <stdexcept>

class Input final : public IInStream, public CMyUnknownImp {
  Z7_IFACES_IMP_UNK_1(IInStream)
  Z7_IFACE_COM7_IMP(ISequentialInStream)
public:
  FILE *file;
  explicit Input(const char *path) : file(std::fopen(path, "rb")) {
    if (!file) throw std::runtime_error("open input");
  }
  ~Input() { std::fclose(file); }
};
Z7_COM7F_IMF(Input::Read(void *data, UInt32 size, UInt32 *processed)) {
  const size_t n = std::fread(data, 1, size, file);
  if (processed) *processed = static_cast<UInt32>(n);
  return std::ferror(file) ? E_FAIL : S_OK;
}
Z7_COM7F_IMF(Input::Seek(Int64 offset, UInt32 origin, UInt64 *position)) {
  if (origin > 2 || fseeko(file, offset, static_cast<int>(origin))) return E_FAIL;
  const auto pos = ftello(file);
  if (pos < 0) return E_FAIL;
  if (position) *position = static_cast<UInt64>(pos);
  return S_OK;
}

class Open final : public IArchiveOpenCallback, public ICryptoGetTextPassword,
                   public CMyUnknownImp {
  Z7_IFACES_IMP_UNK_2(IArchiveOpenCallback, ICryptoGetTextPassword)
public:
  const char *password;
  bool requested = false;
  explicit Open(const char *p) : password(p) {}
};
static HRESULT credential(const char *text, BSTR *out) {
  *out = nullptr;
  if (!text) return E_ABORT;
  // Test fixtures use ASCII credentials only; production must convert Unicode.
  wchar_t wide[128];
  const size_t n = std::strlen(text);
  if (n >= 128) return E_INVALIDARG;
  for (size_t i = 0; i <= n; ++i) wide[i] = static_cast<unsigned char>(text[i]);
  *out = SysAllocStringLen(wide, static_cast<UINT>(n));
  return *out ? S_OK : E_OUTOFMEMORY;
}
Z7_COM7F_IMF(Open::SetTotal(const UInt64 *, const UInt64 *)) { return S_OK; }
Z7_COM7F_IMF(Open::SetCompleted(const UInt64 *, const UInt64 *)) { return S_OK; }
Z7_COM7F_IMF(Open::CryptoGetTextPassword(BSTR *out)) {
  requested = true;
  return credential(password, out);
}

class Sink final : public ISequentialOutStream, public CMyUnknownImp {
  Z7_IFACES_IMP_UNK_1(ISequentialOutStream)
public:
  UInt64 *count;
  bool cancel;
  Sink(UInt64 *n, bool c) : count(n), cancel(c) {}
};
Z7_COM7F_IMF(Sink::Write(const void *, UInt32 size, UInt32 *processed)) {
  if (processed) *processed = 0;
  if (cancel) return E_ABORT;
  *count += size;
  if (processed) *processed = size;
  return S_OK;
}

class Extract final : public IArchiveExtractCallback, public ICryptoGetTextPassword,
                      public CMyUnknownImp {
  Z7_IFACES_IMP_UNK_2(IArchiveExtractCallback, ICryptoGetTextPassword)
  Z7_IFACE_COM7_IMP(IProgress)
public:
  const char *password;
  bool cancel;
  UInt64 bytes = 0;
  unsigned results = 0, failures = 0;
  Extract(const char *p, bool c) : password(p), cancel(c) {}
};
Z7_COM7F_IMF(Extract::SetTotal(UInt64)) { return S_OK; }
Z7_COM7F_IMF(Extract::SetCompleted(const UInt64 *)) { return cancel ? E_ABORT : S_OK; }
Z7_COM7F_IMF(Extract::GetStream(UInt32 index, ISequentialOutStream **stream, Int32 mode)) {
  *stream = nullptr;
  std::printf("stream index=%u mode=%d\n", index, mode);
  if (mode == NArchive::NExtract::NAskMode::kExtract) {
    CMyComPtr<ISequentialOutStream> sink = new Sink(&bytes, cancel);
    *stream = sink.Detach();
  }
  return S_OK;
}
Z7_COM7F_IMF(Extract::PrepareOperation(Int32)) { return S_OK; }
Z7_COM7F_IMF(Extract::SetOperationResult(Int32 result)) {
  ++results;
  failures += result != NArchive::NExtract::NOperationResult::kOK;
  std::printf("operation_result=%d\n", result);
  return S_OK;
}
Z7_COM7F_IMF(Extract::CryptoGetTextPassword(BSTR *out)) { return credential(password, out); }

static int run(const char *library, const char *path, const char *password, const char *mode) {
  void *module = dlopen(library, RTLD_NOW | RTLD_LOCAL);
  if (!module) throw std::runtime_error(dlerror());
  // Process-lifetime module; all COM owners are destroyed before process exit.
  const auto count = reinterpret_cast<Func_GetNumberOfFormats>(dlsym(module, "GetNumberOfFormats"));
  const auto property = reinterpret_cast<Func_GetHandlerProperty2>(dlsym(module, "GetHandlerProperty2"));
  const auto create = reinterpret_cast<Func_CreateObject>(dlsym(module, "CreateObject"));
  const auto moduleProp = reinterpret_cast<Func_GetModuleProp>(dlsym(module, "GetModuleProp"));
  if (!count || !property || !create || !moduleProp) throw std::runtime_error("missing exports");
  PROPVARIANT prop{};
  moduleProp(NModulePropID::kVersion, &prop);
  std::printf("version=%u.%02u wchar=%zu variant=%zu\n", prop.ulVal >> 16,
              prop.ulVal & 0xffff, sizeof(wchar_t), sizeof(PROPVARIANT));
  VariantClear(&prop);
  moduleProp(NModulePropID::kInterfaceType, &prop);
  std::printf("interface_type=%u\n", prop.ulVal);
  VariantClear(&prop);
  UInt32 formats = 0;
  if (count(&formats) != S_OK) throw std::runtime_error("enumerate formats");
  std::printf("formats=%u\n", formats);
  for (UInt32 i = 0; i < formats; ++i) {
    GUID clsid{};
    if (property(i, NArchive::NHandlerPropID::kClassID, &prop) != S_OK || prop.vt != VT_BSTR
        || SysStringByteLen(prop.bstrVal) != sizeof(clsid)) throw std::runtime_error("class id");
    std::memcpy(&clsid, prop.bstrVal, sizeof(clsid));
    VariantClear(&prop);
    CMyComPtr<IInArchive> archive;
    if (create(&clsid, &IID_IInArchive, reinterpret_cast<void **>(&archive)) != S_OK) continue;
    CMyComPtr<IInStream> input = new Input(path);
    Open *openState = new Open(password);
    CMyComPtr<IArchiveOpenCallback> open = openState;
    const UInt64 offset = 0; // Fixture probe only, not a complete auto-open policy.
    const HRESULT opened = archive->Open(input, &offset, open);
    if (opened != S_OK) {
      archive->Close();
      if (openState->requested) {
        std::printf("open_hresult=0x%08x password_requested=1\n", static_cast<unsigned>(opened));
        return 3;
      }
      continue;
    }
    property(i, NArchive::NHandlerPropID::kName, &prop);
    std::printf("format=%ls\n", prop.bstrVal);
    VariantClear(&prop);
    UInt32 items = 0;
    archive->GetNumberOfItems(&items);
    std::printf("items=%u\n", items);
    for (UInt32 item = 0; item < items; ++item) {
      archive->GetProperty(item, kpidPath, &prop);
      std::printf("item=%u name=%ls\n", item, prop.vt == VT_BSTR ? prop.bstrVal : L"");
      VariantClear(&prop);
    }
    Extract *state = new Extract(password, std::strcmp(mode, "cancel") == 0);
    CMyComPtr<IArchiveExtractCallback> callback = state;
    const UInt32 selected = 0;
    const bool test = std::strcmp(mode, "test") == 0;
    const HRESULT hr = archive->Extract(test ? nullptr : &selected, test ? 0xffffffff : 1,
                                        test ? 1 : 0, callback);
    std::printf("extract_hresult=0x%08x results=%u failures=%u bytes=%llu\n",
                static_cast<unsigned>(hr), state->results, state->failures,
                static_cast<unsigned long long>(state->bytes));
    archive->Close();
    return hr == S_OK && state->failures == 0 ? 0 : 4;
  }
  throw std::runtime_error("no handler accepted fixture");
}

int main(int argc, char **argv) {
  if (argc < 3) return 2;
  std::setlocale(LC_ALL, "C.UTF-8");
  try {
    return run(argv[1], argv[2], argc > 3 && std::strcmp(argv[3], "-") ? argv[3] : nullptr,
               argc > 4 ? argv[4] : "test");
  } catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return 5;
  }
}
