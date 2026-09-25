#!/usr/bin/env ruby
# frozen_string_literal: true

# verify_xcodeproj.rb - prints the targets and build phases of app/ios/Runner.xcodeproj and checks
# the invariants add_broadcast_extension.rb establishes. Exits non-zero when one is violated, so it
# can run in CI (Linux or macOS; needs only `gem install xcodeproj`).
#
#   ruby app/ios/scripts/verify_xcodeproj.rb

require 'xcodeproj'

IOS_DIR = File.expand_path('..', __dir__)
project = Xcodeproj::Project.open(File.join(IOS_DIR, 'Runner.xcodeproj'))
errors = []
check = ->(ok, message) { errors << message unless ok }

project.targets.each do |target|
  puts "target #{target.name} (#{target.product_type})"
  target.build_phases.each do |phase|
    detail = case phase
             when Xcodeproj::Project::Object::PBXShellScriptBuildPhase
               phase.shell_script.strip
             when Xcodeproj::Project::Object::PBXCopyFilesBuildPhase
               "dstSubfolderSpec=#{phase.dst_subfolder_spec}: " +
               phase.files_references.map(&:display_name).join(', ')
             else
               phase.files_references.compact.map(&:display_name).join(', ')
             end
    puts "  #{phase.display_name.ljust(34)} #{detail}"
  end
  deps = target.dependencies.map { |d| d.target&.name }.compact
  puts "  depends on: #{deps.join(', ')}" unless deps.empty?
  target.build_configurations.each do |config|
    s = config.build_settings
    puts "  [#{config.name}] bundle=#{s['PRODUCT_BUNDLE_IDENTIFIER']} " \
         "entitlements=#{s['CODE_SIGN_ENTITLEMENTS']} ios=#{s['IPHONEOS_DEPLOYMENT_TARGET']}"
  end
end

runner = project.targets.find { |t| t.name == 'Runner' }
ext = project.targets.find { |t| t.name == 'HfaBroadcast' }
check.call(runner, 'Runner target missing')
check.call(ext, 'HfaBroadcast target missing')

if runner && ext
  check.call(ext.product_type == 'com.apple.product-type.app-extension', 'HfaBroadcast is not an app extension')
  check.call(runner.dependencies.any? { |d| d.target == ext }, 'Runner does not depend on HfaBroadcast')

  embed = runner.copy_files_build_phases.find { |p| p.name == 'Embed Foundation Extensions' }
  check.call(embed, 'Runner has no "Embed Foundation Extensions" phase')
  if embed
    check.call(embed.dst_subfolder_spec == '13', 'embed phase dstSubfolderSpec is not 13 (PlugIns)')
    check.call(embed.files_references.include?(ext.product_reference), 'HfaBroadcast.appex is not embedded')
    names = runner.build_phases.map(&:display_name)
    thin = names.index('Thin Binary')
    check.call(thin.nil? || names.index(embed.display_name) < thin,
               'embed phase must come before "Thin Binary" (Xcode build cycle)')
  end

  sources = ->(t) { t.source_build_phase.files_references.map(&:path) }
  %w[AppDelegate.swift HfaPlatformChannel.swift BroadcastPickerFactory.swift HfaShared.swift].each do |f|
    check.call(sources.call(runner).include?(f), "Runner does not compile #{f}")
  end
  %w[SampleHandler.swift PcmInterleaver.swift HfaShared.swift].each do |f|
    check.call(sources.call(ext).include?(f), "HfaBroadcast does not compile #{f}")
  end
  tests = project.targets.find { |t| t.name == 'RunnerTests' }
  check.call(tests && sources.call(tests).include?('PcmInterleaver.swift'),
             'RunnerTests does not compile PcmInterleaver.swift')
  check.call(!sources.call(runner).include?('PcmInterleaver.swift'), 'Runner must not compile PcmInterleaver.swift')
  %w[AppDelegate.swift HfaPlatformChannel.swift].each do |f|
    check.call(!sources.call(ext).include?(f), "HfaBroadcast must not compile #{f}")
  end

  # App Store Connect rejects binaries whose required-reason API use is not declared.
  { runner => 'Runner', ext => 'HfaBroadcast' }.each do |target, dir|
    manifest = target.resources_build_phase.files_references.find { |f| f.path == 'PrivacyInfo.xcprivacy' }
    check.call(manifest && File.exist?(File.join(IOS_DIR, dir, 'PrivacyInfo.xcprivacy')) &&
               manifest.real_path.to_s == File.join(IOS_DIR, dir, 'PrivacyInfo.xcprivacy'),
               "#{target.name} does not copy #{dir}/PrivacyInfo.xcprivacy")
  end

  first = ext.build_phases.first
  check.call(first.is_a?(Xcodeproj::Project::Object::PBXShellScriptBuildPhase) &&
             first.shell_script.include?('build_rust_ext.sh'),
             'the first HfaBroadcast phase must run scripts/build_rust_ext.sh')

  project_configs = project.build_configurations.map(&:name).sort
  check.call(ext.build_configurations.map(&:name).sort == project_configs,
             "HfaBroadcast configurations differ from the project's #{project_configs}")

  ext.build_configurations.each do |config|
    s = config.build_settings
    expect = {
      'PRODUCT_BUNDLE_IDENTIFIER' => '$(HFA_BROADCAST_BUNDLE_ID)',
      'IPHONEOS_DEPLOYMENT_TARGET' => '15.0',
      'SWIFT_OBJC_BRIDGING_HEADER' => 'HfaBroadcast/HfaBroadcast-Bridging-Header.h',
      'CODE_SIGN_ENTITLEMENTS' => 'HfaBroadcast/HfaBroadcast.entitlements',
      'INFOPLIST_FILE' => 'HfaBroadcast/Info.plist',
      'SKIP_INSTALL' => 'YES',
      'APPLICATION_EXTENSION_API_ONLY' => 'YES'
    }
    expect.each do |key, value|
      check.call(s[key] == value, "HfaBroadcast [#{config.name}] #{key} = #{s[key].inspect}, expected #{value}")
    end
    check.call(Array(s['OTHER_LDFLAGS']).include?('-lhfa_ext'), "HfaBroadcast [#{config.name}] does not link -lhfa_ext")
    check.call(Array(s['LIBRARY_SEARCH_PATHS']).any? { |p| p.include?('$(BUILT_PRODUCTS_DIR)') },
               "HfaBroadcast [#{config.name}] LIBRARY_SEARCH_PATHS lacks $(BUILT_PRODUCTS_DIR)")
    check.call(config.base_configuration_reference&.path == 'HfaBroadcast.xcconfig',
               "HfaBroadcast [#{config.name}] is not based on HfaBroadcast.xcconfig")
  end
  runner.build_configurations.each do |config|
    check.call(config.build_settings['CODE_SIGN_ENTITLEMENTS'] == 'Runner/Runner.entitlements',
               "Runner [#{config.name}] lacks Runner/Runner.entitlements")
    check.call(config.build_settings['PRODUCT_BUNDLE_IDENTIFIER'] == '$(HFA_BUNDLE_ID)',
               "Runner [#{config.name}] PRODUCT_BUNDLE_IDENTIFIER is not $(HFA_BUNDLE_ID)")
    base = config.base_configuration_reference&.path.to_s
    check.call(%w[Debug.xcconfig Release.xcconfig Flutter/Debug.xcconfig Flutter/Release.xcconfig].include?(base),
               "Runner [#{config.name}] is not based on Flutter/Debug.xcconfig or Flutter/Release.xcconfig")
    # The app must never link the extension's copy of the Rust library (duplicate symbols with
    # the cargokit pod's libhfa_ffi.a).
    check.call(!Array(config.build_settings['OTHER_LDFLAGS']).include?('-lhfa_ext'),
               "Runner [#{config.name}] must not link -lhfa_ext")
  end

  # Every committed source of the two targets must exist on disk (GeneratedPluginRegistrant.* is
  # written by `flutter pub get` and git-ignored).
  (runner.source_build_phase.files_references + ext.source_build_phase.files_references).uniq.each do |ref|
    next if ref.path.start_with?('GeneratedPluginRegistrant')

    check.call(File.exist?(ref.real_path), "missing file #{ref.real_path}")
  end
end

# Identity (ios/Identity.xcconfig): one place for the bundle ids and the App Group.
identity = File.join(IOS_DIR, 'Identity.xcconfig')
if File.exist?(identity)
  text = File.read(identity)
  %w[HFA_BUNDLE_ID HFA_BROADCAST_BUNDLE_ID HFA_APP_GROUP].each do |name|
    check.call(text.match?(/^#{name}\s*=/), "Identity.xcconfig does not define #{name}")
  end
  check.call(text.include?('#include? "Identity.local.xcconfig"'),
             'Identity.xcconfig does not include the optional Identity.local.xcconfig')
else
  errors << 'missing Identity.xcconfig'
end
{
  'Flutter/Debug.xcconfig' => '#include "../Identity.xcconfig"',
  'Flutter/Release.xcconfig' => '#include "../Identity.xcconfig"',
  'HfaBroadcast/HfaBroadcast.xcconfig' => '#include "../Identity.xcconfig"'
}.each do |path, line|
  file = File.join(IOS_DIR, path)
  check.call(File.exist?(file) && File.read(file).include?(line), "#{path} does not #{line}")
end
%w[Runner/Runner.entitlements HfaBroadcast/HfaBroadcast.entitlements].each do |path|
  file = File.join(IOS_DIR, path)
  check.call(File.exist?(file) && File.read(file).include?('<string>$(HFA_APP_GROUP)</string>'),
             "#{path} does not use the App Group $(HFA_APP_GROUP)")
end
%w[Runner/Info.plist HfaBroadcast/Info.plist].each do |path|
  file = File.join(IOS_DIR, path)
  text = File.exist?(file) ? File.read(file) : ''
  { 'HfaAppGroup' => '$(HFA_APP_GROUP)', 'HfaBroadcastExtension' => '$(HFA_BROADCAST_BUNDLE_ID)' }.each do |key, value|
    check.call(text.match?(%r{<key>#{key}</key>\s*<string>#{Regexp.escape(value)}</string>}),
               "#{path} lacks #{key} = #{value}")
  end
end

# App icon: the committed PNGs are packaging/icon/generate.py's output (packaging/icon/out/ios).
icon_src = File.expand_path('../../packaging/icon/out/ios/AppIcon.appiconset', IOS_DIR)
icon_dst = File.join(IOS_DIR, 'Runner', 'Assets.xcassets', 'AppIcon.appiconset')
if Dir.exist?(icon_src)
  Dir.glob(File.join(icon_src, '*.png')).each do |png|
    target = File.join(icon_dst, File.basename(png))
    check.call(File.exist?(target) && File.binread(target) == File.binread(png),
               "#{File.basename(png)} differs from packaging/icon/out/ios (copy the generated icons)")
  end
end

if errors.empty?
  puts 'verify_xcodeproj: OK'
else
  errors.each { |e| warn "verify_xcodeproj: ERROR #{e}" }
  exit 1
end
