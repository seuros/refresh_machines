# frozen_string_literal: true
require_relative 'lib/refresh_machines/version'

Gem::Specification.new do |spec|
  spec.name = 'refresh_machines'
  spec.version = RefreshMachines::VERSION
  spec.authors = ['Abdelkader Boudih']
  spec.email = ['oss@seuros.com']
  spec.summary = 'Stale-while-revalidate and stale-if-error decisions for Ruby'
  spec.description = 'Classifies cached values as fresh, stale or expired and grants one refresh lease per key. ' \
                     'Stores no values, fetches nothing and evicts nothing.'
  spec.homepage = 'https://github.com/seuros/refresh_machines'
  spec.license = 'MIT'
  spec.required_ruby_version = '>= 4.0.0'
  spec.metadata['source_code_uri'] = spec.homepage
  spec.metadata['rubygems_mfa_required'] = 'true'
  spec.metadata['cargo_crate_name'] = 'refresh_machines_native'
  spec.metadata['cargo_manifest_path'] = 'ext/refresh_machines_native/ffi/Cargo.toml'
  spec.files = Dir.glob(%w[
    lib/**/*.rb sig/**/*.rbs ext/**/*.{rs,rb,toml,md} Cargo.toml
    Cargo.lock README.md CHANGELOG.md LICENSE.txt
  ]).select { |p| File.file?(p) }
    .reject { |p| p.end_with?('/tests.rs') || p.include?('/tests/') }
  spec.require_paths = ['lib']
  spec.extensions = ['ext/refresh_machines_native/extconf.rb']
  spec.add_dependency 'rb_sys', '~> 0.9', '>= 0.9.130'
end
