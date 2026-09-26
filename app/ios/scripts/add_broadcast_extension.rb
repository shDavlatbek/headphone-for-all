#!/usr/bin/env ruby
# frozen_string_literal: true

# add_broadcast_extension.rb - adds the ReplayKit broadcast upload extension "HfaBroadcast" and
# the iOS platform-channel sources to app/ios/Runner.xcodeproj (docs/CONTRACTS.md §8.1, §8.3).
#
# Idempotent: running it again on an already patched project changes nothing (it finds the
# existing groups, files, target, phases and settings and only fixes what differs), so it can be
# re-run after `flutter create .` or an Xcode edit reset part of the project. The resulting
# project.pbxproj is committed.
#
# Usage (any OS with Ruby; no Xcode needed):
#   gem install xcodeproj
#   ruby app/ios/scripts/add_broadcast_extension.rb
#   ruby app/ios/scripts/verify_xcodeproj.rb        # prints and checks the result
#
# What it does:
# - Runner: adds HfaPlatformChannel.swift, BroadcastPickerFactory.swift, HfaBonjourCodec.swift,
#   HfaBonjourDiscovery.swift (native Bonjour discovery backend) and Shared/HfaShared.swift
#   to the Sources phase, CODE_SIGN_ENTITLEMENTS = Runner/Runner.entitlements (App Group) and
#   PRODUCT_BUNDLE_IDENTIFIER = $(HFA_BUNDLE_ID) (ios/Identity.xcconfig).
# - RunnerTests: also compiles HfaBroadcast/PcmInterleaver.swift (unit tests of the converter).
# - HfaBroadcast (com.apple.product-type.app-extension, bundle $(HFA_BROADCAST_BUNDLE_ID), i.e.
#   io.github.shdavlatbek.hfa.broadcast unless Identity.local.xcconfig overrides HFA_BUNDLE_ID):
#   sources SampleHandler.swift, PcmInterleaver.swift, Shared/HfaShared.swift; first build phase
#   "Build Rust sender (hfa-ffi)" runs scripts/build_rust_ext.sh (-> $BUILT_PRODUCTS_DIR/libhfa_ext.a);
#   links libhfa_ext.a plus the system frameworks/libraries the Rust static library needs.
# - Runner and HfaBroadcast copy their PrivacyInfo.xcprivacy (required-reason API declarations).
# - Runner embeds HfaBroadcast.appex with an "Embed Foundation Extensions" copy-files phase
#   (dstSubfolderSpec 13 = PlugIns) placed before Flutter's "Thin Binary" script (avoids Xcode's
#   "Cycle inside Runner" error) and depends on the extension target.

require 'xcodeproj'

IOS_DIR = File.expand_path('..', __dir__)
PROJECT_PATH = File.join(IOS_DIR, 'Runner.xcodeproj')

EXT_NAME = 'HfaBroadcast'
# Bundle ids come from ios/Identity.xcconfig (included by the targets' base configurations).
RUNNER_BUNDLE_ID = '$(HFA_BUNDLE_ID)'
EXT_BUNDLE_ID = '$(HFA_BROADCAST_BUNDLE_ID)'
DEPLOYMENT_TARGET = '15.0'
RUST_PHASE_NAME = 'Build Rust sender (hfa-ffi)'
EMBED_PHASE_NAME = 'Embed Foundation Extensions'
PLUGINS_DST_SUBFOLDER_SPEC = '13'

# Privacy manifest of Runner and HfaBroadcast: the Rust core calls stat/fstat (std::fs metadata,
# FileTimestamp C617.1: files in the app / App Group container) and cpal's Core Audio backend
# mach_absolute_time (SystemBootTime 35F9.1: elapsed time inside the app).
PRIVACY_MANIFEST = 'PrivacyInfo.xcprivacy'
RUNNER_SOURCES = %w[
  HfaPlatformChannel.swift BroadcastPickerFactory.swift HfaBonjourCodec.swift HfaBonjourDiscovery.swift
].freeze
EXT_SOURCES = %w[SampleHandler.swift PcmInterleaver.swift].freeze
EXT_OTHER_FILES = %w[
  Info.plist HfaBroadcast.entitlements HfaBroadcast-Bridging-Header.h HfaBroadcast.xcconfig
].freeze

# System frameworks the extension links. The Rust static library does not carry its
# dependencies' #[link] directives; this is `rustc --print native-static-libs` of
# `hfa-ffi --no-default-features` for aarch64-apple-ios (AVFAudio, AudioToolbox, CoreAudio,
# Foundation, CoreFoundation, objc, iconv; libopus is bundled into the archive) plus what the
# Swift code imports (ReplayKit, CoreMedia). Re-check when Apple-side Rust dependencies change.
EXT_FRAMEWORKS = %w[
  AVFAudio AudioToolbox CoreAudio CoreFoundation CoreMedia Foundation ReplayKit
].freeze
EXT_OTHER_LDFLAGS = ['$(inherited)', '-lhfa_ext', '-lobjc', '-liconv'].freeze

# Build settings of the extension target, shared by every configuration.
EXT_SETTINGS = {
  'APPLICATION_EXTENSION_API_ONLY' => 'YES',
  'CLANG_ENABLE_MODULES' => 'YES',
  'CODE_SIGN_ENTITLEMENTS' => "#{EXT_NAME}/#{EXT_NAME}.entitlements",
  'CODE_SIGN_STYLE' => 'Automatic',
  'CURRENT_PROJECT_VERSION' => '$(FLUTTER_BUILD_NUMBER)',
  'DEAD_CODE_STRIPPING' => 'YES',
  'ENABLE_BITCODE' => 'NO',
  'ENABLE_USER_SCRIPT_SANDBOXING' => 'NO',
  'GENERATE_INFOPLIST_FILE' => 'NO',
  'HEADER_SEARCH_PATHS' => ['$(inherited)', '"$(PROJECT_DIR)/../../core/hfa-ffi/include"'],
  'INFOPLIST_FILE' => "#{EXT_NAME}/Info.plist",
  'IPHONEOS_DEPLOYMENT_TARGET' => DEPLOYMENT_TARGET,
  'LD_RUNPATH_SEARCH_PATHS' => [
    '$(inherited)', '@executable_path/Frameworks', '@executable_path/../../Frameworks'
  ],
  'LIBRARY_SEARCH_PATHS' => ['$(inherited)', '"$(BUILT_PRODUCTS_DIR)"'],
  'MARKETING_VERSION' => '$(FLUTTER_BUILD_NAME)',
  'OTHER_LDFLAGS' => EXT_OTHER_LDFLAGS,
  'PRODUCT_BUNDLE_IDENTIFIER' => EXT_BUNDLE_ID,
  'PRODUCT_NAME' => '$(TARGET_NAME)',
  'SDKROOT' => 'iphoneos',
  'SKIP_INSTALL' => 'YES',
  'SUPPORTED_PLATFORMS' => 'iphoneos iphonesimulator',
  'SWIFT_OBJC_BRIDGING_HEADER' => "#{EXT_NAME}/#{EXT_NAME}-Bridging-Header.h",
  'SWIFT_VERSION' => '5.0',
  'TARGETED_DEVICE_FAMILY' => '1,2'
}.freeze

# Per-configuration extras (Profile is a release build in Flutter).
EXT_DEBUG_SETTINGS = {
  'DEBUG_INFORMATION_FORMAT' => 'dwarf',
  'ONLY_ACTIVE_ARCH' => 'YES',
  'SWIFT_ACTIVE_COMPILATION_CONDITIONS' => 'DEBUG',
  'SWIFT_OPTIMIZATION_LEVEL' => '-Onone'
}.freeze
EXT_RELEASE_SETTINGS = {
  'DEBUG_INFORMATION_FORMAT' => 'dwarf-with-dsym',
  'SWIFT_COMPILATION_MODE' => 'wholemodule',
  'SWIFT_OPTIMIZATION_LEVEL' => '-O',
  'VALIDATE_PRODUCT' => 'YES'
}.freeze

def log(message)
  puts "add_broadcast_extension: #{message}"
end

# Returns the child group `name` of `parent` (path `path`), creating it when missing.
def ensure_group(parent, name, path)
  group = parent.children.find { |c| c.isa == 'PBXGroup' && c.display_name == name }
  return group if group

  log("group #{name}")
  parent.new_group(name, path)
end

# Returns the file reference `name` inside `group`, creating it when missing.
def ensure_file(group, name)
  ref = group.files.find { |f| f.path == name }
  return ref if ref

  log("file #{group.display_name}/#{name}")
  group.new_reference(name)
end

# Adds the privacy manifest `group/PrivacyInfo.xcprivacy` to the target's Resources phase (App Store
# Connect requires the required-reason APIs the binary uses to be declared, see PRIVACY_MANIFEST).
def ensure_privacy_manifest(target, group)
  ref = ensure_file(group, PRIVACY_MANIFEST)
  ref.last_known_file_type = 'text.xml'
  return if target.resources_build_phase.files_references.include?(ref)

  log("#{target.name}: resource #{PRIVACY_MANIFEST}")
  target.resources_build_phase.add_file_reference(ref, true)
end

# Adds `ref` to the target's Sources phase unless it is there already.
def ensure_source(target, ref)
  return if target.source_build_phase.files_references.include?(ref)

  log("#{target.name}: compile #{ref.path}")
  target.source_build_phase.add_file_reference(ref, true)
end

# Returns an SDK-relative framework reference (the form Xcode itself creates).
def ensure_framework(project, name)
  group = ensure_group(project.frameworks_group, 'iOS', nil)
  path = "System/Library/Frameworks/#{name}.framework"
  ref = group.files.find { |f| f.path == path && f.source_tree == 'SDKROOT' }
  ref || group.new_reference(path, :sdk_root).tap { |r| r.name = "#{name}.framework" }
end

def set_settings(config, settings)
  settings.each do |key, value|
    next if config.build_settings[key] == value

    config.build_settings[key] = value.is_a?(Array) ? value.dup : value
  end
end

project = Xcodeproj::Project.open(PROJECT_PATH)
runner = project.targets.find { |t| t.name == 'Runner' } or abort('Runner target not found')
main = project.main_group

# --- Shared sources (compiled into the app and the extension) --------------------------------
shared_group = ensure_group(main, 'Shared', 'Shared')
shared_swift = ensure_file(shared_group, 'HfaShared.swift')

# --- Runner ----------------------------------------------------------------------------------
runner_group = main.children.find { |c| c.display_name == 'Runner' } or abort('Runner group not found')
RUNNER_SOURCES.each { |name| ensure_source(runner, ensure_file(runner_group, name)) }
ensure_source(runner, shared_swift)
ensure_privacy_manifest(runner, runner_group)
ensure_file(runner_group, 'Runner.entitlements')
runner.build_configurations.each do |config|
  set_settings(config, 'CODE_SIGN_ENTITLEMENTS' => 'Runner/Runner.entitlements',
                       'PRODUCT_BUNDLE_IDENTIFIER' => RUNNER_BUNDLE_ID)
end

# --- Extension target ------------------------------------------------------------------------
ext = project.targets.find { |t| t.name == EXT_NAME }
unless ext
  log("target #{EXT_NAME}")
  ext = project.new_target(:app_extension, EXT_NAME, :ios, DEPLOYMENT_TARGET, project.products_group,
                           :swift)
  # new_target links a Foundation reference with a versioned developer-dir path; the SDK-relative
  # references below replace it.
  ext.frameworks_build_phase.files.dup.each do |build_file|
    ref = build_file.file_ref
    ext.frameworks_build_phase.remove_build_file(build_file)
    ref&.remove_from_project
  end
end
ext.product_name = EXT_NAME

ext_group = ensure_group(main, EXT_NAME, EXT_NAME)
EXT_SOURCES.each { |name| ensure_source(ext, ensure_file(ext_group, name)) }
ensure_source(ext, shared_swift)
ensure_privacy_manifest(ext, ext_group)
# The PCM converter is unit-tested in RunnerTests (an app extension cannot host tests).
runner_tests = project.targets.find { |t| t.name == 'RunnerTests' }
ensure_source(runner_tests, ensure_file(ext_group, 'PcmInterleaver.swift')) if runner_tests
xcconfig = nil
EXT_OTHER_FILES.each do |name|
  ref = ensure_file(ext_group, name)
  xcconfig = ref if name.end_with?('.xcconfig')
end

EXT_FRAMEWORKS.each do |name|
  ref = ensure_framework(project, name)
  next if ext.frameworks_build_phase.files_references.include?(ref)

  log("#{EXT_NAME}: link #{name}.framework")
  ext.frameworks_build_phase.add_file_reference(ref, true)
end

# Every project configuration (Debug, Release, Profile) must exist for the extension too:
# `flutter build` passes -configuration to xcodebuild for all targets.
project.build_configurations.each do |project_config|
  next if ext.build_configuration_list[project_config.name]

  log("#{EXT_NAME}: configuration #{project_config.name}")
  config = project.new(Xcodeproj::Project::Object::XCBuildConfiguration)
  config.name = project_config.name
  config.build_settings = {}
  ext.build_configuration_list.build_configurations << config
end
ext.build_configurations.each do |config|
  config.base_configuration_reference = xcconfig
  set_settings(config, EXT_SETTINGS)
  set_settings(config, config.name == 'Debug' ? EXT_DEBUG_SETTINGS : EXT_RELEASE_SETTINGS)
end

# Rust static library: first build phase of the extension.
rust_phase = ext.shell_script_build_phases.find { |p| p.name == RUST_PHASE_NAME }
unless rust_phase
  log("#{EXT_NAME}: run script #{RUST_PHASE_NAME}")
  rust_phase = ext.new_shell_script_build_phase(RUST_PHASE_NAME)
end
rust_phase.shell_path = '/bin/sh'
rust_phase.shell_script = "/bin/sh \"$PROJECT_DIR/scripts/build_rust_ext.sh\"\n"
rust_phase.always_out_of_date = '1'
rust_phase.show_env_vars_in_log = '0'
rust_phase.input_paths = []
rust_phase.output_paths = ['$(BUILT_PRODUCTS_DIR)/libhfa_ext.a']
unless ext.build_phases.first == rust_phase
  ext.build_phases.delete(rust_phase)
  ext.build_phases.unshift(rust_phase)
end

# --- Embed the extension in Runner ----------------------------------------------------------
embed = runner.copy_files_build_phases.find { |p| p.name == EMBED_PHASE_NAME }
unless embed
  log("Runner: #{EMBED_PHASE_NAME}")
  embed = runner.new_copy_files_build_phase(EMBED_PHASE_NAME)
end
embed.dst_subfolder_spec = PLUGINS_DST_SUBFOLDER_SPEC
embed.dst_path = ''
embed.run_only_for_deployment_postprocessing = '0'
unless embed.files_references.include?(ext.product_reference)
  build_file = embed.add_file_reference(ext.product_reference, true)
  build_file.settings = { 'ATTRIBUTES' => ['RemoveHeadersOnCopy'] }
end
thin_binary = runner.build_phases.find { |p| p.display_name == 'Thin Binary' }
if thin_binary
  runner.build_phases.delete(embed)
  runner.build_phases.insert(runner.build_phases.index(thin_binary), embed)
end

unless runner.dependencies.any? { |d| d.target == ext }
  log("Runner: depends on #{EXT_NAME}")
  runner.add_dependency(ext)
end

attributes = project.root_object.attributes['TargetAttributes'] ||= {}
attributes[ext.uuid] ||= { 'CreatedOnToolsVersion' => '16.0' }

project.save

# xcodeproj names a local Swift package reference by its basename in the pbxproj comments, Xcode
# (and Flutter's Swift Package Manager migration, which writes these lines) by its relative path.
# Comments are ignored by every parser; restoring Xcode's form keeps the diff minimal.
pbxproj = File.join(PROJECT_PATH, 'project.pbxproj')
text = File.read(pbxproj)
project.root_object.package_references.each do |ref|
  next unless ref.isa == 'XCLocalSwiftPackageReference'

  text = text.gsub(%(XCLocalSwiftPackageReference "#{File.basename(ref.relative_path)}" */),
                   %(XCLocalSwiftPackageReference "#{ref.relative_path}" */))
end
File.write(pbxproj, text)
log("saved #{PROJECT_PATH}")
