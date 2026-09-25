#ifndef RUNNER_SINGLE_INSTANCE_H_
#define RUNNER_SINGLE_INSTANCE_H_

#include <windows.h>

// Holds a named mutex for the lifetime of the process so that only one
// instance of the app runs per user session.
//
// Usage in wWinMain:
//   SingleInstanceGuard guard(kSingleInstanceMutexName);
//   if (!guard.IsFirstInstance()) {
//     ActivateRunningInstance(kAppWindowClassName, activate_message, 5000);
//     return EXIT_SUCCESS;
//   }
class SingleInstanceGuard {
 public:
  // Creates (or opens) the mutex |mutex_name|. The process is the first
  // instance unless the mutex already existed or exists in a security context
  // this process may not open (for example an elevated first instance). Any
  // other failure lets the process run, so the app never refuses to start
  // because of the guard itself.
  explicit SingleInstanceGuard(const wchar_t* mutex_name);
  ~SingleInstanceGuard();

  SingleInstanceGuard(const SingleInstanceGuard&) = delete;
  SingleInstanceGuard& operator=(const SingleInstanceGuard&) = delete;

  // Whether this process is the first (and so the only) instance.
  bool IsFirstInstance() const { return first_instance_; }

 private:
  HANDLE mutex_ = nullptr;
  bool first_instance_ = true;
};

// Returns the id of the registered window message |name|, or 0 when it could
// not be registered.
UINT RegisterActivateMessage(const wchar_t* name);

// Asks the running instance to show its main window and bring it to the front.
//
// Looks for a top-level window of class |window_class| (hidden windows
// included, since the app may sit in the tray) for up to |timeout_ms|, which
// covers a first instance that is still starting. It then allows that process
// to take the foreground and posts |activate_message| to the window; the
// window handles it on its own thread (Win32Window::BringToFront). Returns
// whether the message was posted.
bool ActivateRunningInstance(const wchar_t* window_class,
                             UINT activate_message,
                             DWORD timeout_ms);

#endif  // RUNNER_SINGLE_INSTANCE_H_
