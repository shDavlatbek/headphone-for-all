import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/bootstrap.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel(NativeChannel.methodChannelName);
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  test('without a native side every call is a graceful no-op', () async {
    final native = NativeChannel(eventsSupported: true);
    expect(await native.getDataDir(), isNull);
    expect(
      await native.startSystemCapture(
        feedId: 1,
        sampleRate: 48000,
        channels: 2,
      ),
      isFalse,
    );
    await native.stopSystemCapture();
    await native.startHubService();
    await native.stopHubService();
    await native.acquireMulticastLock();
    await native.releaseMulticastLock();
    await native.writeBroadcastConfig(
      const BroadcastConfig(hubHost: 'h', hubPort: 1, label: 'l'),
    );
    expect(await native.captureSupport(), isNull);
    expect(await native.isAvailable(), isFalse);
    // Never listens to the missing event channel.
    expect(await native.events.toList(), isEmpty);
  });

  test('sends the contract method names and arguments', () async {
    final calls = <MethodCall>[];
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call);
      return switch (call.method) {
        'getDataDir' => '/data/hfa',
        'startSystemCapture' => true,
        'captureSupport' => {'supported': true, 'reason': 'broadcast'},
        _ => null,
      };
    });
    final native = NativeChannel(eventsSupported: false);
    expect(await native.getDataDir(), '/data/hfa');
    expect(await native.isAvailable(), isTrue);
    expect(
      await native.startSystemCapture(
        feedId: 1,
        sampleRate: 48000,
        channels: 2,
      ),
      isTrue,
    );
    await native.writeBroadcastConfig(
      const BroadcastConfig(
        hubHost: '10.0.0.2',
        hubPort: 47810,
        hubDeviceId: 'dev',
        hubKey: 'key',
        label: 'iPhone',
      ),
    );
    final support = await native.captureSupport();
    expect(support?.supported, isTrue);
    expect(support?.reason, 'broadcast');

    expect(calls.map((c) => c.method), [
      'getDataDir',
      'startSystemCapture',
      'writeBroadcastConfig',
      'captureSupport',
    ]);
    expect(calls[1].arguments, {
      'feedId': 1,
      'sampleRate': 48000,
      'channels': 2,
    });
    expect(calls[2].arguments, {
      'hubHost': '10.0.0.2',
      'hubPort': 47810,
      'hubDeviceId': 'dev',
      'hubKey': 'key',
      'label': 'iPhone',
    });
  });

  test('platform errors are not swallowed', () async {
    messenger.setMockMethodCallHandler(channel, (call) async {
      throw PlatformException(code: 'denied', message: 'no');
    });
    final native = NativeChannel(eventsSupported: false);
    await expectLater(
      native.startHubService(),
      throwsA(isA<PlatformException>()),
    );
  });

  test('parses native events', () {
    expect(
      NativeEvent.fromMap({'type': 'captureError', 'message': 'boom'}),
      const NativeEvent(NativeEventType.captureError, message: 'boom'),
    );
    expect(
      NativeEvent.fromMap({'type': 'broadcastStarted'}).type,
      NativeEventType.broadcastStarted,
    );
    expect(NativeEvent.fromMap('junk').type, NativeEventType.unknown);
    expect(
      NativeEvent.fromMap({'type': 'other'}).type,
      NativeEventType.unknown,
    );
  });

  test('initCore prefers the native data directory', () async {
    messenger.setMockMethodCallHandler(channel, (call) async {
      return call.method == 'getDataDir' ? '/native/hfa' : null;
    });
    final fake = _DirRecordingFake();
    final info = await initCore(
      api: fake,
      native: NativeChannel(eventsSupported: false),
      loadRust: false,
    );
    expect(fake.dataDir, '/native/hfa');
    expect(info.deviceName, 'Test device');
  });
}

class _DirRecordingFake extends FakeHfaApi {
  String? dataDir;

  @override
  Future<AppInfo> initApp({required String dataDir, String? deviceName}) {
    this.dataDir = dataDir;
    return super.initApp(dataDir: dataDir, deviceName: deviceName);
  }
}
