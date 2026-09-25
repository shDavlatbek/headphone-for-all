import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import 'core_providers.dart';

/// Where the hub's pairing window is.
enum PairingPhase {
  /// No window open.
  idle,

  /// `hubStartPairing` is in flight.
  opening,

  /// PIN and QR are shown; waiting for a sender.
  waiting,

  /// A sender paired (the window closed).
  completed,

  /// The window could not be opened.
  failed,

  /// The hub stopped (which closes the window) while it was shown.
  hubStopped,
}

/// State of the "Pair a device" sheet.
@immutable
class PairingState {
  /// Creates a state.
  const PairingState({
    this.phase = PairingPhase.idle,
    this.info,
    this.message,
    this.pairedName,
  });

  /// Current phase.
  final PairingPhase phase;

  /// The open window (PIN, URI, expiry) while [PairingPhase.waiting].
  final PairingInfoDto? info;

  /// A failed attempt's reason (the window stays open for more attempts) or
  /// why the window could not be opened.
  final String? message;

  /// Name of the device that paired.
  final String? pairedName;

  /// Seconds left before the window closes (never negative).
  int secondsLeft(DateTime now) {
    final expires = info?.expiresAtUnix;
    if (expires == null) return 0;
    final left = expires - now.millisecondsSinceEpoch ~/ 1000;
    return left < 0 ? 0 : left;
  }
}

/// The hub's pairing window.
final pairingControllerProvider =
    NotifierProvider<PairingController, PairingState>(PairingController.new);

/// Opens and closes pairing windows; `HubController` forwards the
/// `PairingCompleted` / `PairingFailed` events here.
class PairingController extends Notifier<PairingState> {
  /// Bumped by every [start], [cancel] and [reset]: a `hubStartPairing` that
  /// resolves after its attempt was superseded must not show its window.
  int _generation = 0;

  @override
  PairingState build() => const PairingState();

  /// Opens a new pairing window.
  Future<void> start() async {
    final generation = ++_generation;
    state = const PairingState(phase: PairingPhase.opening);
    try {
      final info = await ref.read(hfaApiProvider).hubStartPairing();
      if (!ref.mounted) return;
      if (generation != _generation) {
        // Cancelled while opening: the core's cancel may have run first, so
        // close the window this call opened. (A newer start owns the window
        // now and is left alone.)
        final newerStart =
            state.phase == PairingPhase.opening ||
            state.phase == PairingPhase.waiting;
        if (!newerStart) await _cancelInCore();
        return;
      }
      state = PairingState(phase: PairingPhase.waiting, info: info);
    } catch (e) {
      if (!ref.mounted || generation != _generation) return;
      state = PairingState(
        phase: PairingPhase.failed,
        message: describeError(e),
      );
    }
  }

  /// Closes the window (if one is open) and resets the state.
  Future<void> cancel() async {
    if (!ref.mounted) return;
    _generation++;
    final wasOpen =
        state.phase == PairingPhase.waiting ||
        state.phase == PairingPhase.opening;
    state = const PairingState();
    if (!wasOpen) return;
    await _cancelInCore();
  }

  Future<void> _cancelInCore() async {
    try {
      await ref.read(hfaApiProvider).hubCancelPairing();
    } catch (e) {
      debugPrint('cancel pairing: ${describeError(e)}');
    }
  }

  /// The hub stopped: its window is gone (no core call). A sheet that is
  /// still open says so instead of waiting forever.
  void reset() {
    _generation++;
    state = const PairingState(phase: PairingPhase.hubStopped);
  }

  /// A sender paired.
  void onCompleted({required String deviceId, required String name}) {
    state = PairingState(phase: PairingPhase.completed, pairedName: name);
  }

  /// A pairing attempt failed; the window stays open (up to 5 attempts).
  void onFailed(String reason) {
    if (state.phase != PairingPhase.waiting) return;
    state = PairingState(
      phase: PairingPhase.waiting,
      info: state.info,
      message: reason,
    );
  }
}
