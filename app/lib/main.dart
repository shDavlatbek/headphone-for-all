// Minimal first app: loads the Rust core and shows this device's identity.
// feat/app replaces the UI (see docs/CONTRACTS.md §8.2).

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api/app.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(HfaApp(appInfo: loadAppInfo()));
}

/// Initializes the Rust core in `<application support>/hfa` and returns the device info.
Future<AppInfo> loadAppInfo() async {
  final support = await getApplicationSupportDirectory();
  final dataDir = '${support.path}${Platform.pathSeparator}hfa';
  return initApp(dataDir: dataDir);
}

/// Root widget: shows [appInfo] once the core is initialized, or the error.
class HfaApp extends StatelessWidget {
  const HfaApp({super.key, required this.appInfo});

  /// Result of [loadAppInfo] (injected so tests can pass a fake).
  final Future<AppInfo> appInfo;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Headphone for All',
      theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
      home: Scaffold(
        appBar: AppBar(title: const Text('Headphone for All')),
        body: FutureBuilder<AppInfo>(
          future: appInfo,
          builder: (context, snapshot) {
            if (snapshot.hasError) {
              return _Message(
                icon: Icons.error_outline,
                text: 'Could not start the audio core:\n${snapshot.error}',
              );
            }
            final info = snapshot.data;
            if (info == null) {
              return const Center(child: CircularProgressIndicator());
            }
            return AppInfoView(info: info);
          },
        ),
      ),
    );
  }
}

/// Shows the fields of an [AppInfo].
class AppInfoView extends StatelessWidget {
  const AppInfoView({super.key, required this.info});

  /// The info to show.
  final AppInfo info;

  @override
  Widget build(BuildContext context) {
    final caps = info.capabilities;
    final rows = <(String, String)>[
      ('Device name', info.deviceName),
      ('Device id', info.deviceId),
      ('Platform', info.platform),
      ('Version', info.version),
      ('System audio capture', _yesNo(caps.systemMix)),
      ('Per-app capture', _yesNo(caps.perApp)),
      ('Native capture only', _yesNo(caps.externalOnly)),
    ];
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        for (final (label, value) in rows)
          ListTile(title: Text(label), subtitle: SelectableText(value)),
        if (caps.notes.isNotEmpty)
          ListTile(title: const Text('Notes'), subtitle: Text(caps.notes)),
      ],
    );
  }

  static String _yesNo(bool value) => value ? 'yes' : 'no';
}

class _Message extends StatelessWidget {
  const _Message({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 48),
            const SizedBox(height: 12),
            Text(text, textAlign: TextAlign.center),
          ],
        ),
      ),
    );
  }
}
