# Shakedown briefs (1.6.14): four lanes, four builders

You are one lane's builder, a subagent in an isolated git
worktree. Your lane is named in your spawn prompt: a (identity and
the jail), b (seccomp), c (SELinux), or d (the hash chain). Read
your lane's section, the shared protocol, and tasks/PHASE1.md
1.6.14 -- the task, its rulings, and the convergence order are
law. docs/LIFECYCLE.md "The security boundary" is the table this
work turns fully "enforced".

## Protocol (every lane, do not skip)

- Build in your worktree only. Do NOT commit, do NOT tick the
  board, do NOT touch another lane's surface. When your lane's
  gate is green, write a report (what changed, why, the gate's
  output) and finish: the reviewer merges the lanes serially in
  the order a, c, d, b -- one reviewed, signed commit per lane.
- Talk first: an unclear ruling goes to the reviewer in your
  report or before you build on a guess. Never invent a ruling.
- Negative tests are mandatory: what must not happen gets
  asserted, not assumed. Every lane's gate below names its
  negatives.
- Run gates with make targets; gate binaries are target/smoke/*
  (the lab flavor). The field flavor is target/release/*.
- Stray VMMs are named cella-vmm; `pkill -9 -x cella-vmm` and
  remove /tmp/cella-* sandboxes before rerunning a failed gate.
- KVM batteries do not run concurrently across lanes: prove
  `make test` and your lane's own gate in your worktree; the full
  battery runs at the merges, one lane at a time.
- The tree you branch from has the split landed: one binary per
  persona under crates/, the commons in cella-libs behind
  features, the shim owning zero verbs.

## The standing rulings (do not re-ask)

- Identity: a distinct sub-user per machine, mapped by the spawn;
  the SELinux profile binds to THAT user. No shared identity
  anywhere.
- Seccomp: per-binary allowlists; the VMM's shrinks to the run
  loop, and the ioctl filter allows the exact KVM request numbers
  -- anything else kills. The gvisor shape, stricter.
- SELinux: ENFORCING on every host the moment the shakedown
  completes. No permissive-forever dev exception.
- Profiles are files: security/profiles/<cli>/ holds each
  persona's profile as data (the bind set, the namespace set);
  the spawn and jail.sh consume them. The VMM's jail is built by
  the machine persona's spawn (code), reading the same profile
  file.
- The -debug flavor confines like the field flavor, except the
  console surfaces the lab needs (console.sock, enter): the lab
  is an instrument, not an exemption.

## Lane a: identity and the jail (one fused lane)

The subuid mapping lives inside the bwrap invocation, thus one
edit surface: the spawn in cella-libs, jail.sh, and
security/profiles/.

- Each machine runs as its own sub-user; the spawn maps it. Pick
  the mapping mechanics (subuid ranges via newuidmap, or a direct
  uid_map write where the range is delegated) and state the
  choice and its host prerequisites in the report.
- One bwrap profile file per persona: the exact bind set, the
  exact namespace set, nothing else. Read the persona's verbs;
  strace under the battery where reading is not enough. A verb
  process that runs for milliseconds still runs confined.
- The VMM keeps its existing jail as the floor and tightens.
- Persona wrinkles, in this lane:
  - cella-network holds CAP_NET_ADMIN as a file capability, and a
    user namespace strips file capabilities. THE ONE OPEN RULING:
    no-userns jail, ambient-capability handoff, or thin-and-
    unjailed. Flag it in your report and build the other personas
    first; do not decide it.
  - cella-build runs the toolbox (podman): jail the orchestrator;
    the toolbox boundary stays podman's own. State the scope in
    the profile's comments.
  - The shim's confinement is its exec set and nothing else.
  - cella-probe needs /dev/kvm and a tap, nothing else: calibrate
    the process on it first.
- Lane gate: a cross-machine file touch fails by uid before
  SELinux exists to deny it; a persona runs under its own profile
  alone; a path outside a persona's bind set refuses, proven by a
  test that tries one.

## Lane b: seccomp (merges LAST -- build against the final world)

- The per-binary allowlist, one per persona crate; every entry
  carries a comment naming who needs it (the current filter's
  discipline). The VMM's shrinks to the run loop's needs.
- The KVM ioctl filter lands: the exact request numbers the run
  loop issues, enumerated; any other ioctl kills. socket(2) stays
  the canary where it is one today.
- You merge after a, c, and d: rebase your allowlists on the
  merged tree before your report -- the jail and SELinux
  mechanisms' own syscalls (newuidmap, setcon writes) must be in
  the lists of whoever makes them, and building last makes the
  cross-lane leak die by construction.
- Lane gate: each persona's selftest dies by SIGSYS on a syscall
  outside its own list; a KVM ioctl outside the request set kills
  the VMM; the existing test-seccomp pattern extends per persona.

## Lane c: SELinux (binds to lane a's identity)

- The example policy becomes the enforced policy: one domain per
  persona, bound to the per-machine sub-user from lane a;
  lateral movement between machine directories dies.
- LIFECYCLE.md's one "planned" cell turns "enforced" (the table
  row alone -- the full doc pass is 1.6.7's).
- ENFORCING lands at the merge, every host.
- Lane gate: a cross-machine touch denies with an AVC under
  enforcement, and cella doctor harvest files that denial -- the
  1.6.11 verb meets its purpose; a permissive host is a FAIL of
  this gate, not a variant.

## Lane d: the hash chain

- Field 15 fills, both books: every Audit and Event entry carries
  the SHA-256 of its predecessor's framed bytes; the first entry
  chains from the empty digest. Verification walks the book once.
- A verb surfaces it: `cella doctor verify <vm>` extends to the
  books, and a broken link names the entry where the chain snaps.
- Branch and archive carry the books; the twin's chain forks with
  its history and stays valid.
- Rides with lane c by schedule, not dependency: your worktree is
  independent.
- Lane gate: an intact book verifies end to end; a book with one
  edited entry fails loudly, naming the break; a branched twin's
  book still verifies.

## The join (the reviewer's, not a lane's)

After the four merges: the full battery green under enforcement on
both machines, and the per-persona checklist added to
tasks/PHASE1.md 1.6.14, one line per persona, ticked as each
proves out.
