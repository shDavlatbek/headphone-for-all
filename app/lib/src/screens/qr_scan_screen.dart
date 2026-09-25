import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

/// Opens the camera and returns the first `hfa://pair` QR code found, or
/// `null` when the user goes back. Android and iOS only.
Future<String?> scanPairingQr(BuildContext context) {
  return Navigator.of(
    context,
  ).push<String>(MaterialPageRoute(builder: (context) => const QrScanScreen()));
}

/// Full-screen camera view that scans a hub's pairing QR code.
class QrScanScreen extends StatefulWidget {
  /// Creates the screen.
  const QrScanScreen({super.key});

  @override
  State<QrScanScreen> createState() => _QrScanScreenState();
}

class _QrScanScreenState extends State<QrScanScreen> {
  final _controller = MobileScannerController(
    formats: const [BarcodeFormat.qrCode],
    detectionSpeed: DetectionSpeed.noDuplicates,
  );
  bool _done = false;
  String? _hint;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onDetect(BarcodeCapture capture) {
    if (_done) return;
    for (final barcode in capture.barcodes) {
      final value = barcode.rawValue?.trim();
      if (value == null) continue;
      if (value.toLowerCase().startsWith('hfa://pair')) {
        _done = true;
        Navigator.of(context).pop(value);
        return;
      }
      setState(() => _hint = 'That is not a Headphone for All pairing code.');
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Scan pairing code'),
        actions: [
          IconButton(
            tooltip: 'Torch',
            icon: const Icon(Icons.flashlight_on_outlined),
            onPressed: _controller.toggleTorch,
          ),
        ],
      ),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
            errorBuilder: (context, error) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  'The camera is not available: ${error.errorCode.message}',
                  textAlign: TextAlign.center,
                ),
              ),
            ),
          ),
          Align(
            alignment: Alignment.bottomCenter,
            child: Container(
              width: double.infinity,
              color: Colors.black54,
              padding: const EdgeInsets.all(16),
              child: Text(
                _hint ??
                    'Point the camera at the QR code shown on the hub '
                        '(Hub → Pair a device).',
                textAlign: TextAlign.center,
                style: theme.textTheme.bodyLarge?.copyWith(color: Colors.white),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
