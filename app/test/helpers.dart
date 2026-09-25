import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_riverpod/misc.dart' show Override;
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/app.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';
import 'package:headphone_for_all/src/state/core_providers.dart';
import 'package:headphone_for_all/src/state/navigation.dart';

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
}

/// Provider overrides wiring [fake] (and [native]) into the app.
List<Override> overridesFor(
  FakeHfaApi fake, {
  NativeChannel? native,
  Duration pollInterval = const Duration(hours: 1),
}) => [
  hfaApiProvider.overrideWithValue(fake),
  initialAppInfoProvider.overrideWithValue(fake.appInfo),
  nativeChannelProvider.overrideWithValue(native ?? RecordingNativeChannel()),
  // Polling is exercised in the provider tests; keep widget tests event-driven.
  pollIntervalProvider.overrideWithValue(pollInterval),
];

/// A container for provider unit tests.
ProviderContainer containerFor(
  FakeHfaApi fake, {
  NativeChannel? native,
  Duration pollInterval = const Duration(hours: 1),
}) {
  return ProviderContainer.test(
    overrides: overridesFor(fake, native: native, pollInterval: pollInterval),
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
}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    ProviderScope(
      retry: (retryCount, error) => null,
      overrides: overridesFor(fake, native: native),
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
