# Parallel work: branches, worktrees, and agents

rusty-riff has a few **central registries** that almost every feature touches —
`src/dsp/mod.rs` (`ChainStage`, `Params`, the stage dispatch),
`src/ui/config.rs` (`KNOBS`/`PEDALS` tables + range consts), `src/preset.rs`
(sections), and the UI snapshots / fidelity baseline. Two branches that each add
a knob/param/preset field will conflict in those files.

So we parallelize only work with **disjoint file ownership**, each branch in its
own `git worktree` (separate checkout + separate `target/`), driven by a scoped
opencode agent. The maintainer (or the `integrator` agent) merges serially on
`main`.

## The safe trio (current)

| Branch | Agent | Owns (only) | Merge order |
| ------ | ----- | ----------- | ----------- |
| `chore/bench` | `bench-builder` | `examples/` | 1 |
| `docs/guide` | `docs-writer` | `docs/`, `README.md` | 2 |
| `feat/preset-search` | `ui-builder` | `src/ui/**` (incl. snapshots) | 3 |

No branch in this trio touches `src/dsp/**`, `src/preset.rs`,
`src/ui/config.rs`, or the baseline, so they cannot conflict with each other and
CI's fidelity `--check` stays valid.

> **Status:** the first run of this trio landed on `main` together
> (bench + guide + preset search). Keep the table below as the template for the
> next batch.

## Setting up the worktrees

From the main checkout (replace `../rusty-riff-x` with any path outside this
repo):

```bash
git worktree add ../rusty-riff-bench  -b chore/bench        main
git worktree add ../rusty-riff-docs   -b docs/guide         main
git worktree add ../rusty-riff-search -b feat/preset-search main
```

Then start a separate opencode session **in each worktree**, selecting the
matching agent, e.g.:

```bash
cd ../rusty-riff-bench  && opencode --agent bench-builder
cd ../rusty-riff-docs   && opencode --agent docs-writer
cd ../rusty-riff-search && opencode --agent ui-builder
```

(opencode loads agent files from `.opencode/agent/`; **restart** a session after
editing them.) Each session has its own `target/`, so builds don't race.

## Per-branch workflow

1. Work only inside your owned paths (see the agent brief).
2. Keep the gates green: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo test --all-features`.
3. Commit, then `git push -u origin <branch>` — push-triggered CI runs the
   Linux gates plus the macOS AU + fidelity jobs.
4. **Do not merge or rebase onto `main`**; the integrator does that.

## Integration (serial)

The `integrator` agent (or the maintainer) lands branches on `main`, one at a
time, in the order above:

1. `git checkout main && git pull --ff-only` and `git rebase main <branch>`.
2. Re-run all gates. Re-bless UI snapshots if a UI branch changed them
   (`INSTA_UPDATE=always cargo test --lib 'ui::'`) and **inspect the diff**.
3. Regenerate the fidelity baseline **only** if DSP/preset voicing changed
   (not expected for the safe trio):
   `cargo run --release --example fidelity_render -- --write-baseline docs/fidelity/baseline-synth-48k.toml`
   then `--check`.
4. `git merge --ff-only <branch>` and push. Remove the worktree (`git worktree remove <path>`)
   and delete the branch.

## Adding a workstream

A new workstream is safe to parallelize only if its file set is disjoint from the
others'. Anything that adds a `Params` field, a chain stage, a `KNOBS`/`PEDALS`
row, or a preset section is **registry work**: run it *alone* (or designate one
branch as the registry owner and rebase the others after it lands). Assign one
agent + branch + worktree, add it to the table above, and give the agent a
scoped brief like the existing ones.

## Notes

- Push-triggered CI is the per-branch verification; if it ever stops firing,
  `gh workflow run test.yml` runs it manually.
- The `.claude/skills/add-amp-model|add-pedal|add-bundled-preset` guides are the
  reference for registry-touching work.
