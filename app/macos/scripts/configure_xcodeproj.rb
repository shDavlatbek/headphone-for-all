#!/usr/bin/env ruby
# frozen_string_literal: true

# configure_xcodeproj.rb - adds the macOS platform-channel source (Runner/HfaPlatformChannel.swift)
# to app/macos/Runner.xcodeproj and checks the Runner target (docs/CONTRACTS.md §8.3).
#
# Idempotent; the resulting project.pbxproj is committed. `--check` only verifies (exit 1 on a
# problem) and prints the targets and build phases.
#
#   gem install xcodeproj
#   ruby app/macos/scripts/configure_xcodeproj.rb           # patch + verify
#   ruby app/macos/scripts/configure_xcodeproj.rb --check   # verify only

require 'xcodeproj'

MACOS_DIR = File.expand_path('..', __dir__)
PROJECT_PATH = File.join(MACOS_DIR, 'Runner.xcodeproj')
SOURCES = %w[HfaPlatformChannel.swift].freeze
ENTITLEMENTS = {
  'Debug' => 'Runner/DebugProfile.entitlements',
  'Profile' => 'Runner/DebugProfile.entitlements',
  'Release' => 'Runner/Release.entitlements'
}.freeze

check_only = ARGV.include?('--check')
project = Xcodeproj::Project.open(PROJECT_PATH)
runner = project.targets.find { |t| t.name == 'Runner' } or abort('Runner target not found')
group = project.main_group.children.find { |c| c.display_name == 'Runner' } or abort('Runner group not found')

unless check_only
  changed = false
  SOURCES.each do |name|
    ref = group.files.find { |f| f.path == name } || group.new_reference(name)
    next if runner.source_build_phase.files_references.include?(ref)

    puts "configure_xcodeproj: Runner compiles #{name}"
    runner.source_build_phase.add_file_reference(ref, true)
    changed = true
  end
  if changed
    project.save
    # xcodeproj names a local Swift package reference by its basename in the pbxproj comments,
    # Xcode (and Flutter's Swift Package Manager migration) by its relative path; comments are
    # ignored by every parser, restoring Xcode's form keeps the diff minimal.
    pbxproj = File.join(PROJECT_PATH, 'project.pbxproj')
    text = File.read(pbxproj)
    project.root_object.package_references.each do |ref|
      next unless ref.isa == 'XCLocalSwiftPackageReference'

      text = text.gsub(%(XCLocalSwiftPackageReference "#{File.basename(ref.relative_path)}" */),
                       %(XCLocalSwiftPackageReference "#{ref.relative_path}" */))
    end
    File.write(pbxproj, text)
    puts "configure_xcodeproj: saved #{PROJECT_PATH}"
  end
end

project.targets.each do |target|
  kind = target.respond_to?(:product_type) ? target.product_type : target.isa
  puts "target #{target.name} (#{kind})"
  target.build_phases.each do |phase|
    detail = if phase.is_a?(Xcodeproj::Project::Object::PBXShellScriptBuildPhase)
               phase.shell_script.lines.first.to_s.strip
             else
               phase.files_references.compact.map(&:display_name).join(', ')
             end
    puts "  #{phase.display_name.ljust(28)} #{detail}"
  end
end

errors = []
compiled = runner.source_build_phase.files_references.map(&:path)
(SOURCES + %w[AppDelegate.swift MainFlutterWindow.swift]).each do |name|
  errors << "Runner does not compile #{name}" unless compiled.include?(name)
end
runner.build_configurations.each do |config|
  want = ENTITLEMENTS[config.name]
  have = config.build_settings['CODE_SIGN_ENTITLEMENTS']
  errors << "Runner [#{config.name}] CODE_SIGN_ENTITLEMENTS = #{have.inspect}, expected #{want}" if want && have != want
end
ENTITLEMENTS.values.uniq.each do |path|
  errors << "missing #{path}" unless File.exist?(File.join(MACOS_DIR, path))
end

if errors.empty?
  puts 'configure_xcodeproj: OK'
else
  errors.each { |e| warn "configure_xcodeproj: ERROR #{e}" }
  exit 1
end
