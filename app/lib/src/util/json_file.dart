/// Small JSON files the app keeps next to the core's in the data directory.
library;

import 'dart:convert';
import 'dart:io';

/// Reads [file] as JSON; `null` when it is missing or not valid JSON.
Future<Object?> readJsonFile(File file) async {
  if (!await file.exists()) return null;
  try {
    return jsonDecode(await file.readAsString());
  } on FormatException {
    return null;
  }
}

/// Writes [json] to [file] through a temporary file and a rename, so a
/// crash never leaves half a file.
Future<void> writeJsonFile(File file, Object json) async {
  final tmp = File('${file.path}.tmp');
  await tmp.writeAsString(jsonEncode(json), flush: true);
  await tmp.rename(file.path);
}
