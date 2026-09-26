import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_riverpod/misc.dart' show Override;
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/app.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';
import 'package:headphone_for_all/src/platform/sign_in_launcher.dart';
import 'package:headphone_for_all/src/state/core_providers.dart';
import 'package:headphone_for_all/src/state/navigation.dart';
import 'package:headphone_for_all/src/util/links.dart';

/// Records the links the app opens; answers [opens].
class RecordingLinkOpener {
  /// Whether "the browser" takes the links.
  bool opens = true;

  /// Every link opened, in order.
  final List<Uri> opened = [];

  /// The [LinkOpener] to inject.
  Future<bool> call(Uri uri) async {
    opened.add(uri);
    return opens;
  }
}

/// A [SignInLauncher] in memory.
class MemorySignInLauncher implements SignInLauncher {
  /// Whether the app starts at sign-in.
  bool enabled = false;

  @override
  Future<bool?> isEnabled() async => enabled;

  @override
  Future<void> setEnabled(bool enabled) async => this.enabled = enabled;
}

/// A [NativeChannel] that records calls instead of talking to a platform.
class RecordingNativeChannel extends NativeChannel {
  RecordingNativeChannel({this.captureAllowed = true})
    : super(eventsSupported: false);

  /// What [startSystemCapture] answers.
  bool captureAllowed;

  /// Method names in call order.
  final List<String> calls = [];

  /// Arguments of the last [startSystemCapture].
  Map<String, int>? lastCaptureArgs;

  /// The last broadcast config written.
  BroadcastConfig? lastBroadcastConfig;

  final StreamController<NativeEvent> _events = StreamController.broadcast();

  /// Sends a native event.
  void emit(NativeEvent event) => _events.add(event);

  @override
  Stream<NativeEvent> get events => _events.stream;

  @override
  Future<String?> getDataDir() async {
    calls.add('getDataDir');
    return '/native/hfa';
  }

  @override
  Future<bool> startSystemCapture({
    required int feedId,
    required int sampleRate,
    required int channels,
  }) async {
    calls.add('startSystemCapture');
    lastCaptureArgs = {
      'feedId': feedId,
      'sampleRate': sampleRate,
      'channels': channels,
    };
    return captureAllowed;
  }

  @override
  Future<void> stopSystemCapture() async => calls.add('stopSystemCapture');

  /// What [captureStatus] answers (not recorded in [calls]).
  NativeCaptureStatus captureStatusAnswer = const NativeCaptureStatus(
    running: false,
  );

  @override
  Future<NativeCaptureStatus?> captureStatus() async => captureStatusAnswer;

  @override
  Future<void> startHubService() async => calls.add('startHubService');

  @override
  Future<void> stopHubService() async => calls.add('stopHubService');

  @override
  Future<void> acquireMulticastLock() async =>
      calls.add('acquireMulticastLock');

  @override
  Future<void> releaseMulticastLock() async =>
      calls.add('releaseMulticastLock');

  @override
  Future<void> writeBroadcastConfig(BroadcastConfig config) async {
    calls.add('writeBroadcastConfig');
    lastBroadcastConfig = config;
  }

  /// What [getBroadcastStatus] answers (`null`: not implemented here).
  BroadcastStatus? broadcastStatusAnswer;

  /// How often [getBroadcastStatus] was asked (not recorded in [calls]).
  int broadcastStatusQueries = 0;

  @override
  Future<BroadcastStatus?> getBroadcastStatus() async {
    broadcastStatusQueries++;
    return broadcastStatusAnswer;
  }

  @override
  Future<void> beginStreaming() async => calls.add('beginStreaming');

  @override
  Future<void> endStreaming() async => calls.add('endStreaming');
}

/// Provider overrides wiring [fake] (and [native]) into the app. Links go to
/// [links] (default: a browser that takes every link), starting at sign-in to
/// [signIn] (default: not switchable, as on Windows).
List<Override> overridesFor(
  FakeHfaApi fake, {
  NativeChannel? native,
  Duration pollInterval = const Duration(hours: 1),
  RecordingLinkOpener? links,
  SignInLauncher? signIn,
  String? dataDir,
}) => [
  hfaApiProvider.overrideWithValue(fake),
  initialAppInfoProvider.overrideWithValue(fake.appInfo),
  nativeChannelProvider.overrideWithValue(native ?? RecordingNativeChannel()),
  // Polling is exercised in the provider tests; keep widget tests event-driven.
  pollIntervalProvider.overrideWithValue(pollInterval),
  linkOpenerProvider.overrideWithValue((links ?? RecordingLinkOpener()).call),
  // Never the real autostart folder of the machine running the tests.
  signInLauncherProvider.overrideWithValue(signIn),
  if (dataDir != null) dataDirProvider.overrideWithValue(dataDir),
];

/// A container for provider unit tests.
ProviderContainer containerFor(
  FakeHfaApi fake, {
  NativeChannel? native,
  Duration pollInterval = const Duration(hours: 1),
  String? dataDir,
}) {
  return ProviderContainer.test(
    overrides: overridesFor(
      fake,
      native: native,
      pollInterval: pollInterval,
      dataDir: dataDir,
    ),
    retry: (retryCount, error) => null,
  );
}

/// Pumps the whole app on [fake], showing [section], in a window of [size].
Future<ProviderContainer> pumpApp(
  WidgetTester tester,
  FakeHfaApi fake, {
  NativeChannel? native,
  AppSection section = AppSection.home,
  Size size = const Size(1000, 1800),
  RecordingLinkOpener? links,
  SignInLauncher? signIn,
  List<Override> extra = const [],
}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    ProviderScope(
      retry: (retryCount, error) => null,
      overrides: [
        ...overridesFor(fake, native: native, links: links, signIn: signIn),
        ...extra,
      ],
      child: const HfaApp(),
    ),
  );
  final container = ProviderScope.containerOf(
    tester.element(find.byType(HfaApp)),
  );
  container.read(sectionProvider.notifier).select(section);
  await tester.pumpAndSettle();
  return container;
}

/// Unmounts the app so its providers (and their timers) are disposed.
Future<void> unmount(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox.shrink());
}
