# Run from the repository root with the Ruby standard library on the hosted runner.
require "open3"
require "yaml"

manifest = ".github/actions/mint-app-token/action.yml"
upstream = "actions/create-github-app-token"
wrapper = YAML.safe_load_file(manifest)
steps = wrapper.fetch("runs").fetch("steps")
abort "#{manifest}: expected one mint step" unless steps.size == 1
mint = steps.first
ref = mint.fetch("uses")
pin = ref[/\A#{Regexp.escape(upstream)}@([0-9a-f]{40})\z/, 1]
abort "#{manifest}: expected #{upstream} at a full commit SHA" unless pin

# Each caller supplies its own scope. Dropping an input here could widen it.
inputs = wrapper.fetch("inputs").keys
forwarded = inputs.to_h { |name| [name, "${{ inputs.#{name} }}"] }
abort "#{manifest}: forward every input unchanged" unless mint.fetch("with") == forwarded
outputs = wrapper.fetch("outputs")
%w[token app-slug installation-id].each do |name|
  expected = "${{ steps.#{mint.fetch('id')}.outputs.#{name} }}"
  abort "#{manifest}: forward the upstream #{name} output" unless outputs.dig(name, "value") == expected
end

workflows = Dir[".github/workflows/*.{yml,yaml}"]
actions = Dir.glob("**/action.{yml,yaml}", File::FNM_DOTMATCH) - [manifest]
(workflows + actions).each do |path|
  document = YAML.safe_load_file(path, aliases: true)
  steps = document.fetch("jobs", {}).values.flat_map { |job| job.fetch("steps", []) }
  steps.concat(document.fetch("runs", {}).fetch("steps", []))
  steps.each do |step|
    action = step.fetch("uses", "")
    abort "#{path}: use $/.github/actions/mint-app-token" if action.downcase.start_with?("#{upstream}@")
    next unless action == "$/.github/actions/mint-app-token"

    unknown = step.fetch("with", {}).keys - inputs
    abort "#{path}: unknown mint inputs #{unknown.join(', ')}" unless unknown.empty?
  end
end

tag = File.read(manifest)[/^\s*uses:\s*#{Regexp.escape(ref)}\s+# (v\d+\.\d+\.\d+)[ \t]*$/, 1]
abort "#{manifest}: expected the upstream release tag beside the pin" unless tag
tag_ref = "refs/tags/#{tag}"
refs, status = Open3.capture2("git", "ls-remote", "--exit-code", "https://github.com/#{upstream}.git", tag_ref, "#{tag_ref}^{}")
abort "Could not resolve #{upstream} #{tag}" unless status.success?
commits = refs.lines.to_h { |line| line.split.reverse }
# Annotated tags advertise a tag object and its peeled commit.
commit = commits.fetch("#{tag_ref}^{}") { commits.fetch(tag_ref) }
abort "#{upstream} #{tag} resolves to #{commit}, expected #{pin}" unless commit == pin
puts "#{upstream} #{tag} verified at #{commit}"
