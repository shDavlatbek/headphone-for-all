// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint, type=warning, deprecated_member_use, deprecated_member_use_from_same_package
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'hub.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// GENERATED CODE - DO NOT MODIFY BY HAND
// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$HubEventDto {





@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'HubEventDto()';
}


}

/// @nodoc
class $HubEventDtoCopyWith<$Res>  {
$HubEventDtoCopyWith(HubEventDto _, $Res Function(HubEventDto) __);
}


/// Adds pattern-matching-related methods to [HubEventDto].
extension HubEventDtoPatterns on HubEventDto {
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

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( HubEventDto_SourceAdded value)?  sourceAdded,TResult Function( HubEventDto_SourceRemoved value)?  sourceRemoved,TResult Function( HubEventDto_SourceUpdated value)?  sourceUpdated,TResult Function( HubEventDto_PairingCompleted value)?  pairingCompleted,TResult Function( HubEventDto_PairingFailed value)?  pairingFailed,TResult Function( HubEventDto_Error value)?  error,required TResult orElse(),}){
final _that = this;
switch (_that) {
case HubEventDto_SourceAdded() when sourceAdded != null:
return sourceAdded(_that);case HubEventDto_SourceRemoved() when sourceRemoved != null:
return sourceRemoved(_that);case HubEventDto_SourceUpdated() when sourceUpdated != null:
return sourceUpdated(_that);case HubEventDto_PairingCompleted() when pairingCompleted != null:
return pairingCompleted(_that);case HubEventDto_PairingFailed() when pairingFailed != null:
return pairingFailed(_that);case HubEventDto_Error() when error != null:
return error(_that);case _:
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

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( HubEventDto_SourceAdded value)  sourceAdded,required TResult Function( HubEventDto_SourceRemoved value)  sourceRemoved,required TResult Function( HubEventDto_SourceUpdated value)  sourceUpdated,required TResult Function( HubEventDto_PairingCompleted value)  pairingCompleted,required TResult Function( HubEventDto_PairingFailed value)  pairingFailed,required TResult Function( HubEventDto_Error value)  error,}){
final _that = this;
switch (_that) {
case HubEventDto_SourceAdded():
return sourceAdded(_that);case HubEventDto_SourceRemoved():
return sourceRemoved(_that);case HubEventDto_SourceUpdated():
return sourceUpdated(_that);case HubEventDto_PairingCompleted():
return pairingCompleted(_that);case HubEventDto_PairingFailed():
return pairingFailed(_that);case HubEventDto_Error():
return error(_that);}
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

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( HubEventDto_SourceAdded value)?  sourceAdded,TResult? Function( HubEventDto_SourceRemoved value)?  sourceRemoved,TResult? Function( HubEventDto_SourceUpdated value)?  sourceUpdated,TResult? Function( HubEventDto_PairingCompleted value)?  pairingCompleted,TResult? Function( HubEventDto_PairingFailed value)?  pairingFailed,TResult? Function( HubEventDto_Error value)?  error,}){
final _that = this;
switch (_that) {
case HubEventDto_SourceAdded() when sourceAdded != null:
return sourceAdded(_that);case HubEventDto_SourceRemoved() when sourceRemoved != null:
return sourceRemoved(_that);case HubEventDto_SourceUpdated() when sourceUpdated != null:
return sourceUpdated(_that);case HubEventDto_PairingCompleted() when pairingCompleted != null:
return pairingCompleted(_that);case HubEventDto_PairingFailed() when pairingFailed != null:
return pairingFailed(_that);case HubEventDto_Error() when error != null:
return error(_that);case _:
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

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( SourceDto field0)?  sourceAdded,TResult Function( int streamId)?  sourceRemoved,TResult Function( SourceDto field0)?  sourceUpdated,TResult Function( String deviceId,  String name)?  pairingCompleted,TResult Function( String reason)?  pairingFailed,TResult Function( String message)?  error,required TResult orElse(),}) {final _that = this;
switch (_that) {
case HubEventDto_SourceAdded() when sourceAdded != null:
return sourceAdded(_that.field0);case HubEventDto_SourceRemoved() when sourceRemoved != null:
return sourceRemoved(_that.streamId);case HubEventDto_SourceUpdated() when sourceUpdated != null:
return sourceUpdated(_that.field0);case HubEventDto_PairingCompleted() when pairingCompleted != null:
return pairingCompleted(_that.deviceId,_that.name);case HubEventDto_PairingFailed() when pairingFailed != null:
return pairingFailed(_that.reason);case HubEventDto_Error() when error != null:
return error(_that.message);case _:
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

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( SourceDto field0)  sourceAdded,required TResult Function( int streamId)  sourceRemoved,required TResult Function( SourceDto field0)  sourceUpdated,required TResult Function( String deviceId,  String name)  pairingCompleted,required TResult Function( String reason)  pairingFailed,required TResult Function( String message)  error,}) {final _that = this;
switch (_that) {
case HubEventDto_SourceAdded():
return sourceAdded(_that.field0);case HubEventDto_SourceRemoved():
return sourceRemoved(_that.streamId);case HubEventDto_SourceUpdated():
return sourceUpdated(_that.field0);case HubEventDto_PairingCompleted():
return pairingCompleted(_that.deviceId,_that.name);case HubEventDto_PairingFailed():
return pairingFailed(_that.reason);case HubEventDto_Error():
return error(_that.message);}
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

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( SourceDto field0)?  sourceAdded,TResult? Function( int streamId)?  sourceRemoved,TResult? Function( SourceDto field0)?  sourceUpdated,TResult? Function( String deviceId,  String name)?  pairingCompleted,TResult? Function( String reason)?  pairingFailed,TResult? Function( String message)?  error,}) {final _that = this;
switch (_that) {
case HubEventDto_SourceAdded() when sourceAdded != null:
return sourceAdded(_that.field0);case HubEventDto_SourceRemoved() when sourceRemoved != null:
return sourceRemoved(_that.streamId);case HubEventDto_SourceUpdated() when sourceUpdated != null:
return sourceUpdated(_that.field0);case HubEventDto_PairingCompleted() when pairingCompleted != null:
return pairingCompleted(_that.deviceId,_that.name);case HubEventDto_PairingFailed() when pairingFailed != null:
return pairingFailed(_that.reason);case HubEventDto_Error() when error != null:
return error(_that.message);case _:
  return null;

}
}

}

/// @nodoc


class HubEventDto_SourceAdded extends HubEventDto {
  const HubEventDto_SourceAdded(this.field0): super._();
  

 final  SourceDto field0;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_SourceAddedCopyWith<HubEventDto_SourceAdded> get copyWith => _$HubEventDto_SourceAddedCopyWithImpl<HubEventDto_SourceAdded>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_SourceAdded&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode {
    return Object.hash(runtimeType,field0);
}

@override
String toString() {
    return 'HubEventDto.sourceAdded(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_SourceAddedCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_SourceAddedCopyWith(HubEventDto_SourceAdded value, $Res Function(HubEventDto_SourceAdded) _then) = _$HubEventDto_SourceAddedCopyWithImpl;
@useResult
$Res call({
 SourceDto field0
});




}
/// @nodoc
class _$HubEventDto_SourceAddedCopyWithImpl<$Res>
    implements $HubEventDto_SourceAddedCopyWith<$Res> {
  _$HubEventDto_SourceAddedCopyWithImpl(this._self, this._then);

  final HubEventDto_SourceAdded _self;
  final $Res Function(HubEventDto_SourceAdded) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(HubEventDto_SourceAdded(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as SourceDto,
  ));
}


}

/// @nodoc


class HubEventDto_SourceRemoved extends HubEventDto {
  const HubEventDto_SourceRemoved({required this.streamId}): super._();
  

/// The removed stream.
 final  int streamId;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_SourceRemovedCopyWith<HubEventDto_SourceRemoved> get copyWith => _$HubEventDto_SourceRemovedCopyWithImpl<HubEventDto_SourceRemoved>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_SourceRemoved&&(identical(other.streamId, streamId) || other.streamId == streamId));
}


@override
int get hashCode {
    return Object.hash(runtimeType,streamId);
}

@override
String toString() {
    return 'HubEventDto.sourceRemoved(streamId: $streamId)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_SourceRemovedCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_SourceRemovedCopyWith(HubEventDto_SourceRemoved value, $Res Function(HubEventDto_SourceRemoved) _then) = _$HubEventDto_SourceRemovedCopyWithImpl;
@useResult
$Res call({
 int streamId
});




}
/// @nodoc
class _$HubEventDto_SourceRemovedCopyWithImpl<$Res>
    implements $HubEventDto_SourceRemovedCopyWith<$Res> {
  _$HubEventDto_SourceRemovedCopyWithImpl(this._self, this._then);

  final HubEventDto_SourceRemoved _self;
  final $Res Function(HubEventDto_SourceRemoved) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? streamId = null,}) {
  return _then(HubEventDto_SourceRemoved(
streamId: null == streamId ? _self.streamId : streamId // ignore: cast_nullable_to_non_nullable
as int,
  ));
}


}

/// @nodoc


class HubEventDto_SourceUpdated extends HubEventDto {
  const HubEventDto_SourceUpdated(this.field0): super._();
  

 final  SourceDto field0;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_SourceUpdatedCopyWith<HubEventDto_SourceUpdated> get copyWith => _$HubEventDto_SourceUpdatedCopyWithImpl<HubEventDto_SourceUpdated>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_SourceUpdated&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode {
    return Object.hash(runtimeType,field0);
}

@override
String toString() {
    return 'HubEventDto.sourceUpdated(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_SourceUpdatedCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_SourceUpdatedCopyWith(HubEventDto_SourceUpdated value, $Res Function(HubEventDto_SourceUpdated) _then) = _$HubEventDto_SourceUpdatedCopyWithImpl;
@useResult
$Res call({
 SourceDto field0
});




}
/// @nodoc
class _$HubEventDto_SourceUpdatedCopyWithImpl<$Res>
    implements $HubEventDto_SourceUpdatedCopyWith<$Res> {
  _$HubEventDto_SourceUpdatedCopyWithImpl(this._self, this._then);

  final HubEventDto_SourceUpdated _self;
  final $Res Function(HubEventDto_SourceUpdated) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(HubEventDto_SourceUpdated(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as SourceDto,
  ));
}


}

/// @nodoc


class HubEventDto_PairingCompleted extends HubEventDto {
  const HubEventDto_PairingCompleted({required this.deviceId, required this.name}): super._();
  

/// Sender device id.
 final  String deviceId;
/// Sender name.
 final  String name;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_PairingCompletedCopyWith<HubEventDto_PairingCompleted> get copyWith => _$HubEventDto_PairingCompletedCopyWithImpl<HubEventDto_PairingCompleted>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_PairingCompleted&&(identical(other.deviceId, deviceId) || other.deviceId == deviceId)&&(identical(other.name, name) || other.name == name));
}


@override
int get hashCode {
    return Object.hash(runtimeType,deviceId,name);
}

@override
String toString() {
    return 'HubEventDto.pairingCompleted(deviceId: $deviceId, name: $name)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_PairingCompletedCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_PairingCompletedCopyWith(HubEventDto_PairingCompleted value, $Res Function(HubEventDto_PairingCompleted) _then) = _$HubEventDto_PairingCompletedCopyWithImpl;
@useResult
$Res call({
 String deviceId, String name
});




}
/// @nodoc
class _$HubEventDto_PairingCompletedCopyWithImpl<$Res>
    implements $HubEventDto_PairingCompletedCopyWith<$Res> {
  _$HubEventDto_PairingCompletedCopyWithImpl(this._self, this._then);

  final HubEventDto_PairingCompleted _self;
  final $Res Function(HubEventDto_PairingCompleted) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? deviceId = null,Object? name = null,}) {
  return _then(HubEventDto_PairingCompleted(
deviceId: null == deviceId ? _self.deviceId : deviceId // ignore: cast_nullable_to_non_nullable
as String,name: null == name ? _self.name : name // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class HubEventDto_PairingFailed extends HubEventDto {
  const HubEventDto_PairingFailed({required this.reason}): super._();
  

/// Human-readable reason.
 final  String reason;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_PairingFailedCopyWith<HubEventDto_PairingFailed> get copyWith => _$HubEventDto_PairingFailedCopyWithImpl<HubEventDto_PairingFailed>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_PairingFailed&&(identical(other.reason, reason) || other.reason == reason));
}


@override
int get hashCode {
    return Object.hash(runtimeType,reason);
}

@override
String toString() {
    return 'HubEventDto.pairingFailed(reason: $reason)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_PairingFailedCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_PairingFailedCopyWith(HubEventDto_PairingFailed value, $Res Function(HubEventDto_PairingFailed) _then) = _$HubEventDto_PairingFailedCopyWithImpl;
@useResult
$Res call({
 String reason
});




}
/// @nodoc
class _$HubEventDto_PairingFailedCopyWithImpl<$Res>
    implements $HubEventDto_PairingFailedCopyWith<$Res> {
  _$HubEventDto_PairingFailedCopyWithImpl(this._self, this._then);

  final HubEventDto_PairingFailed _self;
  final $Res Function(HubEventDto_PairingFailed) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? reason = null,}) {
  return _then(HubEventDto_PairingFailed(
reason: null == reason ? _self.reason : reason // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class HubEventDto_Error extends HubEventDto {
  const HubEventDto_Error({required this.message}): super._();
  

/// Human-readable message.
 final  String message;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$HubEventDto_ErrorCopyWith<HubEventDto_Error> get copyWith => _$HubEventDto_ErrorCopyWithImpl<HubEventDto_Error>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is HubEventDto_Error&&(identical(other.message, message) || other.message == message));
}


@override
int get hashCode {
    return Object.hash(runtimeType,message);
}

@override
String toString() {
    return 'HubEventDto.error(message: $message)';
}


}

/// @nodoc
abstract mixin class $HubEventDto_ErrorCopyWith<$Res> implements $HubEventDtoCopyWith<$Res> {
  factory $HubEventDto_ErrorCopyWith(HubEventDto_Error value, $Res Function(HubEventDto_Error) _then) = _$HubEventDto_ErrorCopyWithImpl;
@useResult
$Res call({
 String message
});




}
/// @nodoc
class _$HubEventDto_ErrorCopyWithImpl<$Res>
    implements $HubEventDto_ErrorCopyWith<$Res> {
  _$HubEventDto_ErrorCopyWithImpl(this._self, this._then);

  final HubEventDto_Error _self;
  final $Res Function(HubEventDto_Error) _then;

/// Create a copy of HubEventDto
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? message = null,}) {
  return _then(HubEventDto_Error(
message: null == message ? _self.message : message // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

// dart format on
