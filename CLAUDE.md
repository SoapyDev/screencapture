# screencapture: coding standard

These are rules, not suggestions. Each is stated, then its spirit: what it protects, so it can
be applied where the wording doesn't reach. All new code follows them; existing code is brought
in line when it is touched (the big functions get a dedicated splitting step at the end of the
recording work).

1. **Use `unsafe` only when there is no reasonable safe alternative.** A safe alternative that
   costs no massive performance and offers the same features wins.
   *Spirit:* every `unsafe` is a promise the compiler can't check; make as few as possible, and
   only where the safe way would truly cost the product something.

2. **Justify every `unsafe` and test its assumptions.** Each block carries a `// SAFETY:` comment
   saying why it is sound, and tests exercise the edges of what that comment relies on (sizes,
   lifetimes, who else can touch the memory…).
   *Spirit:* soundness must be explained and checked, not assumed; if an assumption breaks
   later, a test should say so before a user does.

3. **Keep the unsafe surface tiny.** Wrap it in a small safe function as soon as possible, so
   callers never see `unsafe`.
   *Spirit:* the less code sits inside or around `unsafe`, the less there is to get wrong and to
   re-check when anything changes.

4. **Keep functions small: aim for 10 lines, never more than 20.** A struct or enum definition
   counts as one line, and so does a `match` on an enum.
   *Spirit:* a function should do one thing that can be read and understood at a glance; length
   is a signal it is doing several.

5. **Fail early.** Check preconditions and return errors at the top, then do the work.
   *Spirit:* the happy path reads straight down without nesting, and bad input never gets far.

6. **Performance is central.** Weigh it in every design decision; measure when unsure.
   *Spirit:* this tool exists to be instant; a slower design needs a good reason and numbers.

7. **Small footprint when idle.** Between captures, keep only the memory that buys a faster
   first capture; give everything else back.
   *Spirit:* a resident daemon must be a good neighbour; memory held must earn its keep.

8. **No recursion.** Use loops and explicit stacks.
   *Spirit:* depth and cost stay bounded and visible; no stack overflow on unusual input.

9. **Descriptive function names.** Small functions whose names say what they do.
   *Spirit:* code reads like a description of the behaviour; the name is the documentation.

10. **Descriptive variable names.** No cryptic abbreviations.
    *Spirit:* a reader never has to decode what a value is.

11. **Favour traits** for shared behaviour and for seams between parts.
    *Spirit:* parts depend on capabilities, not concrete types, so they can be swapped and tested
    with stand-ins.

12. **Favour enums** to model states and choices, over flags and loose values.
    *Spirit:* impossible states can't be represented, and the compiler checks every case.

13. **Type every value that leaves its function.** A `String`, integer, `bool`, `f32`, etc. that
    outlives the function that made it gets its own type (newtype or enum) enforcing its limits.
    Bare primitives only inside one function.
    *Spirit:* limits and meaning are guaranteed once, where the value is made, instead of being
    re-checked (or forgotten) everywhere it travels.

14. **TDD.** Write the failing test first, see it fail, then write the code.
    *Spirit:* every behaviour is pinned by a test that is known to catch its absence.

15. **Run cargo's tools before every commit:** `cargo fmt --all`, `cargo clippy --workspace
    --all-targets` (also with `--no-default-features --features x11,capture,editing,recording`,
    and with each of `capture`, `editing`, `recording` left out) and `cargo test --workspace`,
    all clean. Clippy's `pedantic` and `nursery` groups are on for every crate, with no lint
    allowed workspace-wide: a real exception is an `#[expect(lint, reason = "…")]` on the one
    item it covers (number conversions go through core's `units`, which states each limit once).
    *Spirit:* nothing is committed that the toolchain could have flagged, on any build.

16. **Limit repetition within a slice.** When the same thing appears more than twice within one
    vertical slice (one feature: recording, the overlay, capture…), refactor it into one place.
    Never refactor across two different slices just to share code.
    *Spirit:* not strict DRY: less repetition where it is cheap and local, without coupling
    features together, so a change in one can't cause a regression in another.

17. **One test, one thing.** Each test checks a single behaviour, so it can fail for only one
    reason.
    *Spirit:* a failing test's name says what broke; no digging through a list of asserts to find
    which unrelated expectation went wrong.

18. **No `unwrap`.** Where a panic is truly the right outcome, use `expect` with a message saying
    why it can't happen (or why stopping is correct). Otherwise handle the case: `?`, `let …
    else`, `unwrap_or…`, `map_or…`. Enforced by clippy's `unwrap_used` outside tests; tests
    move to `expect` when they are touched.
    *Spirit:* every possible panic is a decision someone made on purpose and wrote down, never an
    accident left in by habit.

19. **End-to-end tests cover every feature.** Branch-based: each user-facing feature and each
    way of reaching it (command line, daemon, overlay, standalone…) is exercised at least once
    against the real binary.
    *Spirit:* unit tests prove the parts; the e2e suite proves the product a user actually runs,
    so no feature can silently break between the parts.

20. **Avoid cloning; borrow.** Pass references where ownership isn't needed, and take
    `Option<&T>` rather than `&Option<T>`. Clone only to cross a thread or outlive the owner,
    and then at the last moment.
    *Spirit:* copies cost time and memory (rule 6) and blur who owns what; borrowing keeps both
    cheap and clear.

Toolchain: nightly with Polonius (`rust-toolchain.toml`, `-Zpolonius=next` via
`profile-rustflags`).
