# Follow-ups (deferred from v1)

1. **Registry submission: moonrepo third-party plugin registry** — submit an entry once released.
2. **More inferred tasks: `pack`, `watch`, `clean`** — `pack` needs an intent signal
   (`IsPackable` defaults to true for every classlib, so "pack if packable" would spam pack
   tasks); `watch` (`dotnet watch`, persistent + local) and `clean` are trivial but low-value.
   Also consider a config option to override the pinned build configuration (e.g.
   `taskConfiguration: 'Release'`), and a real-moon e2e harness in CI (cache hit/miss/cascade
   round-trip — currently verified manually and by the sandbox tests).
3. **Per-TFM evaluation for multi-targeted projects** — evaluation currently uses the outer
   (cross-targeting) build, so TFM-conditional references/packages are invisible (pinned by the
   `matrix` fixture, documented in the README). Re-evaluating once per `TargetFrameworks` entry
   with `-p:TargetFramework=` would capture them, at N× the evaluation cost; the batched
   traversal makes that affordable, but the union semantics need a decision first (a dependency
   that only exists for `net472` is a real graph edge for moon, but not for every task).
4. **Eval-result caching across graph builds** — the batched traversal evaluates 60 trivial
   projects in ~3.6s cold (soak test, `--ignored soak`), which is fine; a real workspace with
   heavy imports may not be. If cold graph builds hurt, cache per-project evaluation results
   keyed on hashes of that project's restore-relevant files.
5. **`prune_docker`: preserve build outputs like the Rust toolchain does** — `bin`/`obj` are
   removed wholesale; the Rust plugin instead lifts binaries out before deleting `target/`.
   Only worth doing if someone wants `moon docker prune` to keep publish output.

## Deliberately not implemented

- **`sync_project`** — would write `<ProjectReference>` entries into `.csproj` files from moon's
  project graph. That inverts this plugin's direction: MSBuild is the source of truth and
  inference flows out of it. Adding it would create two writers for the same data.
- **NuGet global cache pruning in `prune_docker`** — moon runs a production `install_dependencies`
  *after* prune, so clearing `~/.nuget/packages` forces a full re-download in the same build.
  It's also outside `input.root`, unlike every first-party precedent. Use
  `dotnet nuget locals all --clear` in the Dockerfile instead.
- **`.slnx`/`.sln` member enumeration** — feeding `output.members` from parsed solution files.
  moon's `workspace.yml` owns project discovery; solution files stay dependency-root markers.
  Revisit only if moon grows a use for members.
- **Static XML fast-path for `ProjectReference` parsing** — obsoleted by batched evaluation,
  which pays MSBuild's startup cost once per graph build.
