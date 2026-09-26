// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint, type=warning, deprecated_member_use, deprecated_member_use_from_same_package
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'sender.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// GENERATED CODE - DO NOT MODIFY BY HAND
// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$CaptureSourceDto {





@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'CaptureSourceDto()';
}


}

/// @nodoc
class $CaptureSourceDtoCopyWith<$Res>  {
$CaptureSourceDtoCopyWith(CaptureSourceDto _, $Res Function(CaptureSourceDto) __);
}


/// Adds pattern-matching-related methods to [CaptureSourceDto].
extension CaptureSourceDtoPatterns on CaptureSourceDto {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( CaptureSourceDto_System value)?  system,TResult Function( CaptureSourceDto_SystemExcludingSelf value)?  systemExcludingSelf,TResult Function( CaptureSourceDto_Process value)?  process,TResult Function( CaptureSourceDto_Tone value)?  tone,TResult Function( CaptureSourceDto_External value)?  external_,required TResult orElse(),}){
final _that = this;
switch (_that) {
case CaptureSourceDto_System() when system != null:
return system(_that);case CaptureSourceDto_SystemExcludingSelf() when systemExcludingSelf != null:
return systemExcludingSelf(_that);case CaptureSourceDto_Process() when process != null:
return process(_that);case CaptureSourceDto_Tone() when tone != null:
return tone(_that);case CaptureSourceDto_External() when external_ != null:
return external_(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( CaptureSourceDto_System value)  system,required TResult Function( CaptureSourceDto_SystemExcludingSelf value)  systemExcludingSelf,required TResult Function( CaptureSourceDto_Process value)  process,required TResult Function( CaptureSourceDto_Tone value)  tone,required TResult Function( CaptureSourceDto_External value)  external_,}){
final _that = this;
switch (_that) {
case CaptureSourceDto_System():
return system(_that);case CaptureSourceDto_SystemExcludingSelf():
return systemExcludingSelf(_that);case CaptureSourceDto_Process():
return process(_that);case CaptureSourceDto_Tone():
return tone(_that);case CaptureSourceDto_External():
return external_(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( CaptureSourceDto_System value)?  system,TResult? Function( CaptureSourceDto_SystemExcludingSelf value)?  systemExcludingSelf,TResult? Function( CaptureSourceDto_Process value)?  process,TResult? Function( CaptureSourceDto_Tone value)?  tone,TResult? Function( CaptureSourceDto_External value)?  external_,}){
final _that = this;
switch (_that) {
case CaptureSourceDto_System() when system != null:
return system(_that);case CaptureSourceDto_SystemExcludingSelf() when systemExcludingSelf != null:
return systemExcludingSelf(_that);case CaptureSourceDto_Process() when process != null:
return process(_that);case CaptureSourceDto_Tone() when tone != null:
return tone(_that);case CaptureSourceDto_External() when external_ != null:
return external_(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function()?  system,TResult Function()?  systemExcludingSelf,TResult Function( int pid)?  process,TResult Function( double freqHz)?  tone,TResult Function( int feedId,  int sampleRate,  int channels)?  external_,required TResult orElse(),}) {final _that = this;
switch (_that) {
case CaptureSourceDto_System() when system != null:
return system();case CaptureSourceDto_SystemExcludingSelf() when systemExcludingSelf != null:
return systemExcludingSelf();case CaptureSourceDto_Process() when process != null:
return process(_that.pid);case CaptureSourceDto_Tone() when tone != null:
return tone(_that.freqHz);case CaptureSourceDto_External() when external_ != null:
return external_(_that.feedId,_that.sampleRate,_that.channels);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function()  system,required TResult Function()  systemExcludingSelf,required TResult Function( int pid)  process,required TResult Function( double freqHz)  tone,required TResult Function( int feedId,  int sampleRate,  int channels)  external_,}) {final _that = this;
switch (_that) {
case CaptureSourceDto_System():
return system();case CaptureSourceDto_SystemExcludingSelf():
return systemExcludingSelf();case CaptureSourceDto_Process():
return process(_that.pid);case CaptureSourceDto_Tone():
return tone(_that.freqHz);case CaptureSourceDto_External():
return external_(_that.feedId,_that.sampleRate,_that.channels);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function()?  system,TResult? Function()?  systemExcludingSelf,TResult? Function( int pid)?  process,TResult? Function( double freqHz)?  tone,TResult? Function( int feedId,  int sampleRate,  int channels)?  external_,}) {final _that = this;
switch (_that) {
case CaptureSourceDto_System() when system != null:
return system();case CaptureSourceDto_SystemExcludingSelf() when systemExcludingSelf != null:
return systemExcludingSelf();case CaptureSourceDto_Process() when process != null:
return process(_that.pid);case CaptureSourceDto_Tone() when tone != null:
return tone(_that.freqHz);case CaptureSourceDto_External() when external_ != null:
return external_(_that.feedId,_that.sampleRate,_that.channels);case _:
  return null;

}
}

}

/// @nodoc


class CaptureSourceDto_System extends CaptureSourceDto {
  const CaptureSourceDto_System(): super._();
  






@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto_System);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'CaptureSourceDto.system()';
}


}




/// @nodoc


class CaptureSourceDto_SystemExcludingSelf extends CaptureSourceDto {
  const CaptureSourceDto_SystemExcludingSelf(): super._();
  






@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto_SystemExcludingSelf);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'CaptureSourceDto.systemExcludingSelf()';
}


}




/// @nodoc


class CaptureSourceDto_Process extends CaptureSourceDto {
  const CaptureSourceDto_Process({required this.pid}): super._();
  

/// Process id (from `list_capture_apps`).
 final  int pid;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CaptureSourceDto_ProcessCopyWith<CaptureSourceDto_Process> get copyWith => _$CaptureSourceDto_ProcessCopyWithImpl<CaptureSourceDto_Process>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto_Process&&(identical(other.pid, pid) || other.pid == pid));
}


@override
int get hashCode {
    return Object.hash(runtimeType,pid);
}

@override
String toString() {
    return 'CaptureSourceDto.process(pid: $pid)';
}


}

/// @nodoc
abstract mixin class $CaptureSourceDto_ProcessCopyWith<$Res> implements $CaptureSourceDtoCopyWith<$Res> {
  factory $CaptureSourceDto_ProcessCopyWith(CaptureSourceDto_Process value, $Res Function(CaptureSourceDto_Process) _then) = _$CaptureSourceDto_ProcessCopyWithImpl;
@useResult
$Res call({
 int pid
});




}
/// @nodoc
class _$CaptureSourceDto_ProcessCopyWithImpl<$Res>
    implements $CaptureSourceDto_ProcessCopyWith<$Res> {
  _$CaptureSourceDto_ProcessCopyWithImpl(this._self, this._then);

  final CaptureSourceDto_Process _self;
  final $Res Function(CaptureSourceDto_Process) _then;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? pid = null,}) {
  return _then(CaptureSourceDto_Process(
pid: null == pid ? _self.pid : pid // ignore: cast_nullable_to_non_nullable
as int,
  ));
}


}

/// @nodoc


class CaptureSourceDto_Tone extends CaptureSourceDto {
  const CaptureSourceDto_Tone({required this.freqHz}): super._();
  

/// Frequency in Hz, 0 < f < 24000.
 final  double freqHz;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CaptureSourceDto_ToneCopyWith<CaptureSourceDto_Tone> get copyWith => _$CaptureSourceDto_ToneCopyWithImpl<CaptureSourceDto_Tone>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto_Tone&&(identical(other.freqHz, freqHz) || other.freqHz == freqHz));
}


@override
int get hashCode {
    return Object.hash(runtimeType,freqHz);
}

@override
String toString() {
    return 'CaptureSourceDto.tone(freqHz: $freqHz)';
}


}

/// @nodoc
abstract mixin class $CaptureSourceDto_ToneCopyWith<$Res> implements $CaptureSourceDtoCopyWith<$Res> {
  factory $CaptureSourceDto_ToneCopyWith(CaptureSourceDto_Tone value, $Res Function(CaptureSourceDto_Tone) _then) = _$CaptureSourceDto_ToneCopyWithImpl;
@useResult
$Res call({
 double freqHz
});




}
/// @nodoc
class _$CaptureSourceDto_ToneCopyWithImpl<$Res>
    implements $CaptureSourceDto_ToneCopyWith<$Res> {
  _$CaptureSourceDto_ToneCopyWithImpl(this._self, this._then);

  final CaptureSourceDto_Tone _self;
  final $Res Function(CaptureSourceDto_Tone) _then;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? freqHz = null,}) {
  return _then(CaptureSourceDto_Tone(
freqHz: null == freqHz ? _self.freqHz : freqHz // ignore: cast_nullable_to_non_nullable
as double,
  ));
}


}

/// @nodoc


class CaptureSourceDto_External extends CaptureSourceDto {
  const CaptureSourceDto_External({required this.feedId, required this.sampleRate, required this.channels}): super._();
  

/// Feed id shared with the native side.
 final  int feedId;
/// Sample rate of the pushed PCM (8000..=192000).
 final  int sampleRate;
/// Channels of the pushed PCM (1..=8).
 final  int channels;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$CaptureSourceDto_ExternalCopyWith<CaptureSourceDto_External> get copyWith => _$CaptureSourceDto_ExternalCopyWithImpl<CaptureSourceDto_External>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is CaptureSourceDto_External&&(identical(other.feedId, feedId) || other.feedId == feedId)&&(identical(other.sampleRate, sampleRate) || other.sampleRate == sampleRate)&&(identical(other.channels, channels) || other.channels == channels));
}


@override
int get hashCode {
    return Object.hash(runtimeType,feedId,sampleRate,channels);
}

@override
String toString() {
    return 'CaptureSourceDto.external_(feedId: $feedId, sampleRate: $sampleRate, channels: $channels)';
}


}

/// @nodoc
abstract mixin class $CaptureSourceDto_ExternalCopyWith<$Res> implements $CaptureSourceDtoCopyWith<$Res> {
  factory $CaptureSourceDto_ExternalCopyWith(CaptureSourceDto_External value, $Res Function(CaptureSourceDto_External) _then) = _$CaptureSourceDto_ExternalCopyWithImpl;
@useResult
$Res call({
 int feedId, int sampleRate, int channels
});




}
/// @nodoc
class _$CaptureSourceDto_ExternalCopyWithImpl<$Res>
    implements $CaptureSourceDto_ExternalCopyWith<$Res> {
  _$CaptureSourceDto_ExternalCopyWithImpl(this._self, this._then);

  final CaptureSourceDto_External _self;
  final $Res Function(CaptureSourceDto_External) _then;

/// Create a copy of CaptureSourceDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? feedId = null,Object? sampleRate = null,Object? channels = null,}) {
  return _then(CaptureSourceDto_External(
feedId: null == feedId ? _self.feedId : feedId // ignore: cast_nullable_to_non_nullable
as int,sampleRate: null == sampleRate ? _self.sampleRate : sampleRate // ignore: cast_nullable_to_non_nullable
as int,channels: null == channels ? _self.channels : channels // ignore: cast_nullable_to_non_nullable
as int,
  ));
}


}

/// @nodoc
mixin _$DiscoveryEventDto {





@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is DiscoveryEventDto);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'DiscoveryEventDto()';
}


}

/// @nodoc
class $DiscoveryEventDtoCopyWith<$Res>  {
$DiscoveryEventDtoCopyWith(DiscoveryEventDto _, $Res Function(DiscoveryEventDto) __);
}


/// Adds pattern-matching-related methods to [DiscoveryEventDto].
extension DiscoveryEventDtoPatterns on DiscoveryEventDto {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( DiscoveryEventDto_Found value)?  found,TResult Function( DiscoveryEventDto_Lost value)?  lost,required TResult orElse(),}){
final _that = this;
switch (_that) {
case DiscoveryEventDto_Found() when found != null:
return found(_that);case DiscoveryEventDto_Lost() when lost != null:
return lost(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( DiscoveryEventDto_Found value)  found,required TResult Function( DiscoveryEventDto_Lost value)  lost,}){
final _that = this;
switch (_that) {
case DiscoveryEventDto_Found():
return found(_that);case DiscoveryEventDto_Lost():
return lost(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( DiscoveryEventDto_Found value)?  found,TResult? Function( DiscoveryEventDto_Lost value)?  lost,}){
final _that = this;
switch (_that) {
case DiscoveryEventDto_Found() when found != null:
return found(_that);case DiscoveryEventDto_Lost() when lost != null:
return lost(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( HubInfoDto field0)?  found,TResult Function( String deviceId)?  lost,required TResult orElse(),}) {final _that = this;
switch (_that) {
case DiscoveryEventDto_Found() when found != null:
return found(_that.field0);case DiscoveryEventDto_Lost() when lost != null:
return lost(_that.deviceId);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( HubInfoDto field0)  found,required TResult Function( String deviceId)  lost,}) {final _that = this;
switch (_that) {
case DiscoveryEventDto_Found():
return found(_that.field0);case DiscoveryEventDto_Lost():
return lost(_that.deviceId);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( HubInfoDto field0)?  found,TResult? Function( String deviceId)?  lost,}) {final _that = this;
switch (_that) {
case DiscoveryEventDto_Found() when found != null:
return found(_that.field0);case DiscoveryEventDto_Lost() when lost != null:
return lost(_that.deviceId);case _:
  return null;

}
}

}

/// @nodoc


class DiscoveryEventDto_Found extends DiscoveryEventDto {
  const DiscoveryEventDto_Found(this.field0): super._();
  

 final  HubInfoDto field0;

/// Create a copy of DiscoveryEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$DiscoveryEventDto_FoundCopyWith<DiscoveryEventDto_Found> get copyWith => _$DiscoveryEventDto_FoundCopyWithImpl<DiscoveryEventDto_Found>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is DiscoveryEventDto_Found&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode {
    return Object.hash(runtimeType,field0);
}

@override
String toString() {
    return 'DiscoveryEventDto.found(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $DiscoveryEventDto_FoundCopyWith<$Res> implements $DiscoveryEventDtoCopyWith<$Res> {
  factory $DiscoveryEventDto_FoundCopyWith(DiscoveryEventDto_Found value, $Res Function(DiscoveryEventDto_Found) _then) = _$DiscoveryEventDto_FoundCopyWithImpl;
@useResult
$Res call({
 HubInfoDto field0
});




}
/// @nodoc
class _$DiscoveryEventDto_FoundCopyWithImpl<$Res>
    implements $DiscoveryEventDto_FoundCopyWith<$Res> {
  _$DiscoveryEventDto_FoundCopyWithImpl(this._self, this._then);

  final DiscoveryEventDto_Found _self;
  final $Res Function(DiscoveryEventDto_Found) _then;

/// Create a copy of DiscoveryEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(DiscoveryEventDto_Found(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as HubInfoDto,
  ));
}


}

/// @nodoc


class DiscoveryEventDto_Lost extends DiscoveryEventDto {
  const DiscoveryEventDto_Lost({required this.deviceId}): super._();
  

/// Its device id.
 final  String deviceId;

/// Create a copy of DiscoveryEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$DiscoveryEventDto_LostCopyWith<DiscoveryEventDto_Lost> get copyWith => _$DiscoveryEventDto_LostCopyWithImpl<DiscoveryEventDto_Lost>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is DiscoveryEventDto_Lost&&(identical(other.deviceId, deviceId) || other.deviceId == deviceId));
}


@override
int get hashCode {
    return Object.hash(runtimeType,deviceId);
}

@override
String toString() {
    return 'DiscoveryEventDto.lost(deviceId: $deviceId)';
}


}

/// @nodoc
abstract mixin class $DiscoveryEventDto_LostCopyWith<$Res> implements $DiscoveryEventDtoCopyWith<$Res> {
  factory $DiscoveryEventDto_LostCopyWith(DiscoveryEventDto_Lost value, $Res Function(DiscoveryEventDto_Lost) _then) = _$DiscoveryEventDto_LostCopyWithImpl;
@useResult
$Res call({
 String deviceId
});




}
/// @nodoc
class _$DiscoveryEventDto_LostCopyWithImpl<$Res>
    implements $DiscoveryEventDto_LostCopyWith<$Res> {
  _$DiscoveryEventDto_LostCopyWithImpl(this._self, this._then);

  final DiscoveryEventDto_Lost _self;
  final $Res Function(DiscoveryEventDto_Lost) _then;

/// Create a copy of DiscoveryEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? deviceId = null,}) {
  return _then(DiscoveryEventDto_Lost(
deviceId: null == deviceId ? _self.deviceId : deviceId // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

// dart format on
