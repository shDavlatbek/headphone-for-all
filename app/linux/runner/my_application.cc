#include "my_application.h"

#include <flutter_linux/flutter_linux.h>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

#include "flutter/generated_plugin_registrant.h"

namespace {

// Title of the main window.
constexpr char kWindowTitle[] = "Headphone for All";

// Initial and minimum window sizes in logical pixels.
constexpr int kInitialWidth = 960;
constexpr int kInitialHeight = 680;
constexpr int kMinimumWidth = 380;
constexpr int kMinimumHeight = 520;

// The argument that starts the app hidden in the tray (the autostart entry
// written by the app's "Start at sign-in" switch passes it; see
// lib/src/platform/sign_in_launcher.dart and docs/CONTRACTS.md §8.10).
constexpr char kAutostartArgument[] = "--autostart";

// Whether `arguments` (NULL-terminated, without the program name) contain
// kAutostartArgument.
bool has_autostart_argument(char** arguments) {
  if (arguments == nullptr) {
    return false;
  }
  for (char** arg = arguments; *arg != nullptr; ++arg) {
    if (g_strcmp0(*arg, kAutostartArgument) == 0) {
      return true;
    }
  }
  return false;
}

// Window icon sizes installed into the bundle (data/icons/hicolor, see
// linux/CMakeLists.txt and packaging/icon/generate.py).
constexpr int kIconSizes[] = {16, 24, 32, 48, 64, 128, 256};

// Sets the window icon. The icons of the relocatable bundle
// (<exe dir>/data/icons/hicolor/<N>x<N>/apps/<app id>.png) are used when they
// are present, so the icon also shows when the app is not installed; the
// themed icon named after the application id (installed by the AppImage /
// Flatpak / distribution package) is the fallback. On Wayland the compositor
// takes the icon from the .desktop file matching the application id instead.
void set_window_icon(GtkWindow* window) {
  g_autofree gchar* exe = g_file_read_link("/proc/self/exe", nullptr);
  GList* icons = nullptr;
  if (exe != nullptr) {
    g_autofree gchar* exe_dir = g_path_get_dirname(exe);
    for (int size : kIconSizes) {
      g_autofree gchar* size_dir = g_strdup_printf("%dx%d", size, size);
      g_autofree gchar* path =
          g_build_filename(exe_dir, "data", "icons", "hicolor", size_dir,
                           "apps", APPLICATION_ID ".png", nullptr);
      GdkPixbuf* pixbuf = gdk_pixbuf_new_from_file(path, nullptr);
      if (pixbuf != nullptr) {
        icons = g_list_append(icons, pixbuf);
      }
    }
  }
  if (icons != nullptr) {
    gtk_window_set_icon_list(window, icons);
    g_list_free_full(icons, g_object_unref);
  } else {
    gtk_window_set_icon_name(window, APPLICATION_ID);
  }
}

}  // namespace

struct _MyApplication {
  GtkApplication parent_instance;
  char** dart_entrypoint_arguments;
  // The main window once it was created (owned by GTK; cleared when it is
  // destroyed).
  GtkWindow* window;
  // Started with --autostart: the first frame does not show the window; it
  // stays hidden until the tray (windowManager.show()) or a second launch
  // shows it.
  gboolean start_hidden;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

// Called when first Flutter frame received.
static void first_frame_cb(MyApplication* self, FlView* view) {
  if (self->start_hidden) {
    self->start_hidden = FALSE;
    return;
  }
  gtk_widget_show(gtk_widget_get_toplevel(GTK_WIDGET(view)));
}

// Implements GApplication::activate.
//
// The application is unique per session (D-Bus name = application id): a
// second launch only activates this primary instance and exits. The existing
// window is then shown and raised, also when it was hidden to the tray.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  if (self->window != nullptr) {
    gtk_window_present(self->window);
    return;
  }

  GtkWindow* window =
      GTK_WINDOW(gtk_application_window_new(GTK_APPLICATION(application)));
  self->window = window;
  g_object_add_weak_pointer(G_OBJECT(window),
                            reinterpret_cast<gpointer*>(&self->window));

  // Use a header bar when running in GNOME as this is the common style used
  // by applications and is the setup most users will be using (e.g. Ubuntu
  // desktop).
  // If running on X and not using GNOME then just use a traditional title bar
  // in case the window manager does more exotic layout, e.g. tiling.
  // If running on Wayland assume the header bar will work (may need changing
  // if future cases occur).
  gboolean use_header_bar = TRUE;
#ifdef GDK_WINDOWING_X11
  GdkScreen* screen = gtk_window_get_screen(window);
  if (GDK_IS_X11_SCREEN(screen)) {
    const gchar* wm_name = gdk_x11_screen_get_window_manager_name(screen);
    if (g_strcmp0(wm_name, "GNOME Shell") != 0) {
      use_header_bar = FALSE;
    }
  }
#endif
  if (use_header_bar) {
    GtkHeaderBar* header_bar = GTK_HEADER_BAR(gtk_header_bar_new());
    gtk_widget_show(GTK_WIDGET(header_bar));
    gtk_header_bar_set_title(header_bar, kWindowTitle);
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
  } else {
    gtk_window_set_title(window, kWindowTitle);
  }

  gtk_window_set_default_size(window, kInitialWidth, kInitialHeight);
  GdkGeometry geometry = {};
  geometry.min_width = kMinimumWidth;
  geometry.min_height = kMinimumHeight;
  gtk_window_set_geometry_hints(window, nullptr, &geometry, GDK_HINT_MIN_SIZE);
  gtk_window_set_position(window, GTK_WIN_POS_CENTER);
  set_window_icon(window);

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  fl_dart_project_set_dart_entrypoint_arguments(
      project, self->dart_entrypoint_arguments);

  FlView* view = fl_view_new(project);
  GdkRGBA background_color;
  // Background defaults to black, override it here if necessary, e.g. #00000000
  // for transparent.
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(view, &background_color);
  gtk_widget_show(GTK_WIDGET(view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));

  // Show the window when Flutter renders.
  // Requires the view to be realized so we can start rendering.
  g_signal_connect_swapped(view, "first-frame", G_CALLBACK(first_frame_cb),
                           self);
  gtk_widget_realize(GTK_WIDGET(view));

  fl_register_plugins(FL_PLUGIN_REGISTRY(view));

  gtk_widget_grab_focus(GTK_WIDGET(view));
}

// Implements GApplication::local_command_line.
static gboolean my_application_local_command_line(GApplication* application,
                                                  gchar*** arguments,
                                                  int* exit_status) {
  MyApplication* self = MY_APPLICATION(application);
  // Strip out the first argument as it is the binary name.
  self->dart_entrypoint_arguments = g_strdupv(*arguments + 1);
  const gboolean autostart =
      has_autostart_argument(self->dart_entrypoint_arguments);

  g_autoptr(GError) error = nullptr;
  if (!g_application_register(application, nullptr, &error)) {
    g_warning("Failed to register: %s", error->message);
    *exit_status = 1;
    return TRUE;
  }

  *exit_status = 0;
  // An autostart while the app already runs (e.g. started by hand before
  // the session's autostart ran) leaves the running instance alone.
  if (autostart && g_application_get_is_remote(application)) {
    return TRUE;
  }
  self->start_hidden = autostart;
  g_application_activate(application);

  return TRUE;
}

// Implements GApplication::startup.
static void my_application_startup(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application startup.

  G_APPLICATION_CLASS(my_application_parent_class)->startup(application);
}

// Implements GApplication::shutdown.
static void my_application_shutdown(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application shutdown.

  G_APPLICATION_CLASS(my_application_parent_class)->shutdown(application);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  if (self->window != nullptr) {
    g_object_remove_weak_pointer(G_OBJECT(self->window),
                                 reinterpret_cast<gpointer*>(&self->window));
    self->window = nullptr;
  }
  g_clear_pointer(&self->dart_entrypoint_arguments, g_strfreev);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = my_application_activate;
  G_APPLICATION_CLASS(klass)->local_command_line =
      my_application_local_command_line;
  G_APPLICATION_CLASS(klass)->startup = my_application_startup;
  G_APPLICATION_CLASS(klass)->shutdown = my_application_shutdown;
  G_OBJECT_CLASS(klass)->dispose = my_application_dispose;
}

static void my_application_init(MyApplication* self) {
  self->window = nullptr;
  self->start_hidden = FALSE;
}

MyApplication* my_application_new() {
  // Set the program name to the application ID, which helps various systems
  // like GTK and desktop environments map this running application to its
  // corresponding .desktop file. This ensures better integration by allowing
  // the application to be recognized beyond its binary name.
  g_set_prgname(APPLICATION_ID);

  // Release and profile builds are unique (not G_APPLICATION_NON_UNIQUE like
  // the Flutter template): one instance per session, a second launch raises
  // the first one's window (see my_application_activate). Without a D-Bus
  // session bus GLib falls back to a non-unique instance, so the app still
  // starts. Debug builds (no NDEBUG) stay non-unique like the template, so
  // that `flutter run` and integration tests start their own process instead
  // of activating an installed instance that is running and exiting. The
  // application id (and so the data directory) is the same in every build.
#if !defined(NDEBUG)
  constexpr GApplicationFlags kFlags = G_APPLICATION_NON_UNIQUE;
#elif GLIB_CHECK_VERSION(2, 74, 0)
  constexpr GApplicationFlags kFlags = G_APPLICATION_DEFAULT_FLAGS;
#else
  constexpr GApplicationFlags kFlags = G_APPLICATION_FLAGS_NONE;
#endif
  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, "flags",
                                     kFlags, nullptr));
}
