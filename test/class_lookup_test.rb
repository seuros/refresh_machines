# frozen_string_literal: true

require 'minitest/autorun'
require 'open3'
require 'rbconfig'

# The class of the value the extension wraps (Refresher) is fixed when it
# loads. Nothing a caller does later, redefining the constant or
# `Object.const_get`, reaches a refresher being built. Each case runs in a
# fresh process: the class is fixed once per process, at load.
class ClassLookupTest < Minitest::Test
  LIB = File.expand_path('../lib', __dir__)

  PRELUDE = <<~RUBY
    require 'refresh_machines'
    R = RefreshMachines
  RUBY

  # Runs `body` after PRELUDE in a new Ruby; answers its last line of output.
  def isolated(body)
    out, err, status = Open3.capture3(RbConfig.ruby, '-I', LIB, '-e', PRELUDE + body)
    assert status.success?, "exited #{status.exitstatus}: #{err}"
    out.lines.last&.chomp
  end

  # Bug: a Refresher constant redefined before the first refresher made `new`,
  # `with_scope` and `resumed` answer an instance of the new class, with none
  # of the refresher's methods.
  def test_a_redefined_refresher_constant_does_not_change_the_refresher_class
    assert_equal 'true true true Fetch 1', isolated(<<~RUBY)
      original = R::Refresher
      R.send(:remove_const, :Refresher)
      R.const_set(:Refresher, Class.new)
      built = [original.new, original.with_scope(1), original.resumed(2, 5)]
      fetch = built.first.decide(42, 0, 100)
      puts [*built.map { it.instance_of?(original) }, fetch.class.name.split('::').last,
            built.first.in_flight].join(' ')
    RUBY
  end

  # Bug: building the first refresher ran `Object.const_get`, a caller's code
  # in the middle of a native call. No Ruby runs while one is built now.
  def test_building_a_refresher_runs_no_const_get
    assert_equal '0 Refresher Refresher Refresher', isolated(<<~RUBY)
      hits = 0
      Object.define_singleton_method(:const_get) do |*args|
        hits += 1
        super(*args)
      end
      built = [R::Refresher.new, R::Refresher.with_scope(1), R::Refresher.resumed(2, 5)]
      puts [hits, *built.map { it.class.name.split('::').last }].join(' ')
    RUBY
  end

  # Bug: a redefined `Object.const_get` that raised hit an `unwrap` in the
  # extension: an uncatchable fatal, and the process exited.
  def test_a_raising_const_get_does_not_kill_the_process
    assert_equal 'Fetch 1', isolated(<<~RUBY)
      Object.define_singleton_method(:const_get) { |*| raise IOError, 'lookup ran mid-call' }
      refresher = R::Refresher.with_scope(3)
      fetch = refresher.decide(42, 0, 100)
      puts [fetch.class.name.split('::').last, refresher.in_flight].join(' ')
    RUBY
  end
end
