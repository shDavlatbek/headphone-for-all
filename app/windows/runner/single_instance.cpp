#include "single_instance.h"

#include <sddl.h>

namespace {

// Delay between two lookups of the first instance's window.
constexpr DWORD kPollIntervalMs = 100;

// DACL: full access (GA) for SYSTEM, Administrators and the owner; SYNCHRONIZE
// (0x00100000) for Everyone, which is what OpenMutex in Inno Setup's
// CheckForMutexes asks for. SACL: medium mandatory label with no-write-up, so
// an elevated (high integrity) instance's mutex stays visible to medium
// integrity processes. The name has no "Global\" prefix, so only processes of
// the same session can reach it.
constexpr const wchar_t kMutexSecurityDescriptor[] =
    L"D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x00100000;;;WD)"
    L"S:(ML;;NW;;;ME)";

}  // namespace

SingleInstanceGuard::SingleInstanceGuard(const wchar_t* mutex_name) {
  if (mutex_name == nullptr) {
    return;
  }
  PSECURITY_DESCRIPTOR descriptor = nullptr;
  SECURITY_ATTRIBUTES attributes{};
  attributes.nLength = sizeof(attributes);
  attributes.bInheritHandle = FALSE;
  // Without a descriptor (conversion failed) the default one is used, which
  // still works for non-elevated instances.
  const bool have_descriptor =
      ::ConvertStringSecurityDescriptorToSecurityDescriptorW(
          kMutexSecurityDescriptor, SDDL_REVISION_1, &descriptor, nullptr) !=
      FALSE;
  attributes.lpSecurityDescriptor = have_descriptor ? descriptor : nullptr;
  mutex_ = ::CreateMutexW(&attributes, FALSE, mutex_name);
  DWORD error = ::GetLastError();
  if (descriptor != nullptr) {
    ::LocalFree(descriptor);
  }
  if (mutex_ == nullptr && have_descriptor && error != ERROR_ACCESS_DENIED) {
    // The descriptor itself was refused (e.g. a label above this process's
    // integrity level): fall back to the default one.
    mutex_ = ::CreateMutexW(nullptr, FALSE, mutex_name);
    error = ::GetLastError();
  }
  if (mutex_ == nullptr) {
    // The mutex exists but belongs to a context we may not open (e.g. an
    // elevated instance): another instance is running.
    first_instance_ = error != ERROR_ACCESS_DENIED;
    return;
  }
  first_instance_ = error != ERROR_ALREADY_EXISTS;
}

SingleInstanceGuard::~SingleInstanceGuard() {
  if (mutex_ != nullptr) {
    ::CloseHandle(mutex_);
    mutex_ = nullptr;
  }
}

UINT RegisterActivateMessage(const wchar_t* name) {
  return ::RegisterWindowMessageW(name);
}

bool ActivateRunningInstance(const wchar_t* window_class,
                             UINT activate_message,
                             DWORD timeout_ms) {
  if (activate_message == 0) {
    return false;
  }
  const ULONGLONG deadline = ::GetTickCount64() + timeout_ms;
  HWND window = ::FindWindowW(window_class, nullptr);
  while (window == nullptr && ::GetTickCount64() < deadline) {
    ::Sleep(kPollIntervalMs);
    window = ::FindWindowW(window_class, nullptr);
  }
  if (window == nullptr) {
    return false;
  }

  // This process was just started by the user, so it may hand its right to
  // set the foreground window to the running instance.
  DWORD process_id = 0;
  ::GetWindowThreadProcessId(window, &process_id);
  if (process_id != 0) {
    ::AllowSetForegroundWindow(process_id);
  }
  return ::PostMessageW(window, activate_message, 0, 0) != FALSE;
}
