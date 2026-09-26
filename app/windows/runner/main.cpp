#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>
// shobjidl.h needs the Windows types above.
#include <shobjidl.h>

#include "app_identity.h"
#include "flutter_window.h"
#include "single_instance.h"
#include "utils.h"

namespace {

// How long a second instance waits for the first one's window to appear.
constexpr DWORD kActivateTimeoutMs = 5000;

// Shown by a second launch that could not bring the running instance to the
// front (for example because it is not responding).
constexpr const wchar_t kAlreadyRunningText[] =
    L"Headphone for All is already running.\n\n"
    L"Use its icon in the notification area to show the window or to quit "
    L"the app.";

}  // namespace

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // One instance per user session: a second launch (Start menu, installer's
  // "Launch" box, autostart) brings the running window to the front instead,
  // even when it is hidden in the tray.
  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();
  // Started by the installer's "Start when I sign in" shortcut: stay in the
  // tray.
  const bool autostart =
      HasArgument(command_line_arguments, kAutostartArgument);

  const UINT activate_message = RegisterActivateMessage(kActivateMessageName);
  // Debug builds skip the guard (kEnforceSingleInstance).
  SingleInstanceGuard single_instance(
      kEnforceSingleInstance ? kSingleInstanceMutexName : nullptr);
  if (!single_instance.IsFirstInstance()) {
    // A sign-in launch never pops up an instance that is already running.
    if (!autostart &&
        !ActivateRunningInstance(kAppWindowClassName, activate_message,
                                 kActivateTimeoutMs)) {
      // Never exit silently: the user clicked a shortcut and expects a window.
      ::OutputDebugStringW(
          L"Headphone for All: could not activate the running instance\n");
      ::MessageBoxW(nullptr, kAlreadyRunningText, kAppWindowTitle,
                    MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND);
    }
    return EXIT_SUCCESS;
  }

  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Group the taskbar button, pinned shortcuts and notifications under the
  // app's own id (the installer's shortcuts use the same one). Failure only
  // loses that grouping.
  ::SetCurrentProcessExplicitAppUserModelID(kAppUserModelId);

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  window.SetMinimumSize(
      Win32Window::Size(kMinimumWindowWidth, kMinimumWindowHeight));
  window.SetActivateMessage(activate_message);
  window.SetQuitMessage(::RegisterWindowMessageW(kQuitMessageName));
  window.SetShowOnFirstFrame(!autostart);
  // (0, 0) selects the primary monitor; the window is centred in its work
  // area.
  Win32Window::Point origin(0, 0);
  Win32Window::Size size(kInitialWindowWidth, kInitialWindowHeight);
  if (!window.Create(kAppWindowTitle, origin, size)) {
    return EXIT_FAILURE;
  }
  // The installer and uninstaller quit the app with the quit message, and
  // Restart Manager with WM_ENDSESSION (see Win32Window::MessageHandler).
  // Closing the window quits the app unless the Dart side intercepts the close
  // (window_manager's setPreventClose), which it does while the hub or a
  // sender runs: it then hides the window to the tray (ShowWindow(SW_HIDE)),
  // which keeps this message loop running. Quitting from the tray destroys the
  // window or posts WM_QUIT, both of which end the loop below.
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
