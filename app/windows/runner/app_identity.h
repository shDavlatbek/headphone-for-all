#ifndef RUNNER_APP_IDENTITY_H_
#define RUNNER_APP_IDENTITY_H_

// Names that identify Headphone for All to Windows. They are shared by the
// runner, the installer (packaging/windows/hfa.iss) and docs/CONTRACTS.md, so
// change them everywhere at once.

// Title of the main window.
constexpr const wchar_t kAppWindowTitle[] = L"Headphone for All";

// Window class of the main window. It is unique to this app (the Flutter
// template's generic class name is shared by every Flutter app), so a second
// instance can find the first one's window with FindWindow.
constexpr const wchar_t kAppWindowClassName[] =
    L"io.github.shdavlatbek.hfa.MainWindow";

// Explicit AppUserModelID: groups the taskbar button, pinned shortcuts and
// notifications of every instance. The installer's shortcuts carry it too.
constexpr const wchar_t kAppUserModelId[] = L"io.github.shdavlatbek.hfa";

// Named mutex held by the running instance for its whole lifetime. It lives
// in the session namespace (no "Global\" prefix): one instance per logged-in
// user session. The installer's AppMutex names it to detect a running app.
constexpr const wchar_t kSingleInstanceMutexName[] =
    L"io.github.shdavlatbek.hfa.SingleInstance";

// Registered window message a second instance posts to the first one's main
// window to bring it to the front (also when it is hidden in the tray).
constexpr const wchar_t kActivateMessageName[] =
    L"io.github.shdavlatbek.hfa.Activate";

// Initial and minimum window sizes in logical pixels (scaled by the monitor's
// DPI). The initial size is clamped to the monitor's work area.
constexpr unsigned int kInitialWindowWidth = 960;
constexpr unsigned int kInitialWindowHeight = 680;
constexpr unsigned int kMinimumWindowWidth = 380;
constexpr unsigned int kMinimumWindowHeight = 520;

#endif  // RUNNER_APP_IDENTITY_H_
