# Shakedown instructions (1.6.14)

Ask the operator for which CLI to shakedown. Do not pick one
yourself: the operator names the persona, and one invocation of
these instructions shakes down that one persona, all three layers.

The reference: tasks/PHASE1.md 1.6.14 (the task and its lanes),
docs/LIFECYCLE.md "The security boundary" (the table this work
turns fully "enforced"), and security/profiles/<cli>/ (the empty
homes the profiles fill). Prerequisites: 1.6.13 landed (the binary
holds only its own verbs), and the identity decision ruled (ask
the operator if tasks/PHASE1.md does not record it yet).

## The order: jail, then seccomp, then SELinux

The jail decides what exists for the process. Seccomp decides what
the process can ask. SELinux witnesses both. Therefore:

1. Jail first: the smallest filesystem and namespace set comes
   first, thus every later layer is judged against the smallest
   world.
2. Seccomp second: the allowlist is tuned against the tightened
   jail. A filter tuned before the jail shrinks can be too
   generous, never too strict.
3. SELinux last: the policy describes the finished behavior of
   the other two layers, written once. Denial triage runs on a
   stable base.

Do not reorder. A discovery in a later layer that invalidates an
earlier one (a jail path SELinux needs, a syscall the jail entry
makes) goes back to that earlier layer before continuing.

## What a profile is

Undecided, and the first ruling to ask for alongside the persona:
whether security/profiles/<cli>/ holds bwrap argument files that a
wrapper reads, or the profile compiles into the code that spawns
the process. Do not invent the shape silently; the answer applies
to every persona after the first.

## Persona notes

The layers are uniform; the personas are not. Read the note for
the named persona before layer 1:

- **cella-network**: a capability binary. bwrap's user namespace
  strips file capabilities, thus a naive jail destroys its
  CAP_NET_ADMIN. Its jail leg needs its own ruling first: no user
  namespace, an ambient-capability handoff, or the decision that
  the capability binary stays unjailed and thin. Ask the operator.
- **cella-vmm**: never invoked, always spawned -- its jail lives
  in machine.rs (the spawn builds the bwrap invocation). The
  profile work here means tightening what the spawn binds, and
  the wiring change is code, not a wrapper.
- **cella-build**: runs the toolbox (podman). A container runtime
  inside a bwrap jail is out of scope: jail the orchestrator, and
  the toolbox boundary stays podman's own. State this scope in
  the profile's comments.
- **cella** (the dispatcher): it execs the personas by argv0. Its
  own confinement is the exec set and nothing else; the real
  surface lives in the personas it becomes.
- **The -debug flavor**: field-only confinement unless the
  operator rules otherwise. The lab binaries carry the console by
  design; ask before spending a layer on them.
- **cella-probe**: needs /dev/kvm and a tap, nothing else -- its
  jail is small and honest; a good first persona to calibrate the
  process on.

## Layer 1: the jail

- Write the bwrap profile for the persona into
  security/profiles/<cli>/: the exact bind set, the namespace
  set, and nothing else. Start from what the persona touches
  today (read its verbs; run the battery under strace when the
  reading is not enough) and bind the minimum.
- The VMM persona keeps the existing jail as the floor and
  tightens from it. Verb personas gain a jail where they have
  none: a verb process that runs for milliseconds still runs
  confined.
- Build the identity decision here if this persona is the VMM
  (the ruling from tasks/PHASE1.md: one cella uid, per-VMM subuids, or
  MCS categories).
- Negative gate: a path outside the bind set refuses, proven by
  a test that tries one.

## Layer 2: seccomp

- The per-binary allowlist in src (the VMM's shrinks to the run
  loop's needs; a verb persona gets its own short list). Every
  entry carries a comment naming who needs it, as the current
  filter does.
- The VMM's ioctl filtering on the KVM request set is the
  long-standing candidate: land it here.
- socket(2) stays the canary where it is one today.
- Negative gate: a syscall outside the list kills with SIGSYS,
  proven by the self-test pattern that exists (test-seccomp).

## Layer 3: SELinux

- The example policy becomes the enforced policy for this
  persona's domain: lateral movement between machine directories
  dies.
- The ledger's tamper-evidence hash chain lands with this layer
  by schedule (1.6.11 reserved the field); it has no dependency
  on the policy and may proceed in parallel within this step.
- Negative gate: a cross-machine touch denies under enforcement,
  proven by a test that tries one.

## The join

After the three layers: make test, then the full battery, green
under enforcement, on both machines. The known cross-layer leak --
a jail or SELinux mechanism that needs a syscall the seccomp layer
removed -- fails loudly here; fix it in the layer that owns it.
Update LIFECYCLE.md's confinement table for this persona. tasks/PHASE1.md
1.6.14 has no per-persona checklist yet: add one on the first
shakedown (one line per persona, ticked as each finishes), then
tick this persona. Leave the tree uncommitted for the reviewer.
