#include "single_instance.h"

namespace {

// Delay between two lookups of the first instance's window.
constexpr DWORD kPollIntervalMs = 100;

}  // namespace

SingleInstanceGuard::SingleInstanceGuard(const wchar_t* mutex_name) {
  mutex_ = ::CreateMutexW(nullptr, FALSE, mutex_name);
  const DWORD error = ::GetLastError();
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
