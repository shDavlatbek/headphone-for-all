import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../models/hub_target.dart';

/// Asks for the 6-digit PIN shown on [hubName]. Returns the PIN, or `null`
/// when cancelled.
Future<String?> showPinDialog(BuildContext context, {required String hubName}) {
  return showDialog<String>(
    context: context,
    builder: (context) => _PinDialog(hubName: hubName),
  );
}

class _PinDialog extends StatefulWidget {
  const _PinDialog({required this.hubName});

  final String hubName;

  @override
  State<_PinDialog> createState() => _PinDialogState();
}

class _PinDialogState extends State<_PinDialog> {
  final _controller = TextEditingController();
  bool _valid = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _submit() {
    if (_valid) Navigator.of(context).pop(_controller.text.trim());
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Enter PIN'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'On ${widget.hubName}, open the hub screen and tap '
            '"Pair a device". Type the 6-digit PIN it shows.',
          ),
          const SizedBox(height: 16),
          TextField(
            key: const Key('pin-field'),
            controller: _controller,
            autofocus: true,
            keyboardType: TextInputType.number,
            maxLength: 6,
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.headlineSmall?.copyWith(
              letterSpacing: 8,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
            inputFormatters: [FilteringTextInputFormatter.digitsOnly],
            decoration: const InputDecoration(
              hintText: '000000',
              counterText: '',
            ),
            onChanged: (text) => setState(() => _valid = isValidPin(text)),
            onSubmitted: (_) => _submit(),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _valid ? _submit : null,
          child: const Text('Pair'),
        ),
      ],
    );
  }
}

/// Asks for a hub address (host, optional port and PIN).
Future<HubTarget?> showAddHubDialog(BuildContext context) {
  return showDialog<HubTarget>(
    context: context,
    builder: (context) => const _AddHubDialog(),
  );
}

class _AddHubDialog extends StatefulWidget {
  const _AddHubDialog();

  @override
  State<_AddHubDialog> createState() => _AddHubDialogState();
}

class _AddHubDialogState extends State<_AddHubDialog> {
  final _form = GlobalKey<FormState>();
  final _host = TextEditingController();
  final _port = TextEditingController();
  final _pin = TextEditingController();

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _pin.dispose();
    super.dispose();
  }

  void _submit() {
    if (!(_form.currentState?.validate() ?? false)) return;
    // `host:port` and `[v6]:port` (as the hub screen copies them) are split;
    // a value in the Port field wins over a port in the host text.
    final address = splitHostPort(_host.text);
    if (address == null) return;
    Navigator.of(context).pop(
      HubTarget.manual(
        host: address.host,
        port: int.tryParse(_port.text.trim()) ?? address.port ?? 0,
        pin: _pin.text,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Add hub by address'),
      content: Form(
        key: _form,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextFormField(
                key: const Key('host-field'),
                controller: _host,
                autofocus: true,
                decoration: const InputDecoration(
                  labelText: 'Host or IP address',
                  hintText: '192.168.1.20 or 192.168.1.20:47810',
                ),
                validator: (v) {
                  if (v == null || v.trim().isEmpty) {
                    return 'Enter the hub address';
                  }
                  return splitHostPort(v) == null
                      ? 'Enter a host, an IP address or host:port'
                      : null;
                },
              ),
              TextFormField(
                key: const Key('port-field'),
                controller: _port,
                keyboardType: TextInputType.number,
                inputFormatters: [FilteringTextInputFormatter.digitsOnly],
                decoration: const InputDecoration(
                  labelText: 'Port (optional)',
                  hintText: '47810',
                ),
                validator: validatePort,
              ),
              TextFormField(
                key: const Key('manual-pin-field'),
                controller: _pin,
                keyboardType: TextInputType.number,
                maxLength: 6,
                inputFormatters: [FilteringTextInputFormatter.digitsOnly],
                decoration: const InputDecoration(
                  labelText: 'PIN (if not paired yet)',
                ),
                validator: (v) {
                  final text = v?.trim() ?? '';
                  return text.isEmpty || isValidPin(text)
                      ? null
                      : 'The PIN has 6 digits';
                },
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(onPressed: _submit, child: const Text('Add')),
      ],
    );
  }
}

/// Asks for a pasted `hfa://pair?...` link. Returns the trimmed text.
Future<String?> showPairingLinkDialog(BuildContext context) {
  return showDialog<String>(
    context: context,
    builder: (context) => const _PairingLinkDialog(),
  );
}

class _PairingLinkDialog extends StatefulWidget {
  const _PairingLinkDialog();

  @override
  State<_PairingLinkDialog> createState() => _PairingLinkDialogState();
}

class _PairingLinkDialogState extends State<_PairingLinkDialog> {
  final _controller = TextEditingController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text != null && mounted) setState(() => _controller.text = text);
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Use a pairing link'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const Text(
            'On the hub, tap "Pair a device" and copy the link under the QR '
            'code.',
          ),
          const SizedBox(height: 12),
          TextField(
            key: const Key('link-field'),
            controller: _controller,
            decoration: InputDecoration(
              labelText: 'hfa://pair?…',
              suffixIcon: IconButton(
                tooltip: 'Paste',
                icon: const Icon(Icons.content_paste),
                onPressed: _paste,
              ),
            ),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () {
            final text = _controller.text.trim();
            if (text.isNotEmpty) Navigator.of(context).pop(text);
          },
          child: const Text('Use'),
        ),
      ],
    );
  }
}

/// A yes/no confirmation. Returns `true` when confirmed.
Future<bool> showConfirmDialog(
  BuildContext context, {
  required String title,
  required String message,
  required String confirmLabel,
}) async {
  final result = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: Text(message),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: Text(confirmLabel),
        ),
      ],
    ),
  );
  return result ?? false;
}

/// Shows [message] in a snack bar.
void showMessage(
  BuildContext context,
  String message, {
  SnackBarAction? action,
}) {
  ScaffoldMessenger.maybeOf(context)
    ?..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(message), action: action));
}

/// Validates an optional port field: empty (any / default port) or 1–65535.
/// Returns the error text, or `null` when valid.
String? validatePort(String? value) {
  final text = value?.trim() ?? '';
  if (text.isEmpty) return null;
  final port = int.tryParse(text);
  return (port == null || port < 1 || port > 65535)
      ? 'Port must be 1–65535'
      : null;
}
