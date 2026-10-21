# iLO 3 original OS + real-time dependency analysis — is an RTOS actually needed?

Date: 2026-10-21. Static RE of `decompiled/os_main_base0.bin` (app) +
`kernel_type2.bin` (BSP). Answers the design question for the Rust rewrite:
**does any iLO 3 function need hard real-time, i.e. a preemptive RTOS with
deterministic priority scheduling — or is it an event-driven system that an async
executor can replace?** Consolidates the OS findings from docs/02/04/10/12.

## DECISION (2026-10-21): embassy, no RTOS

**Chosen execution model = `embassy` cooperative async, no preemptive RTOS.** The audit
below shows no iLO 3 function needs hard real-time, so the determinism argument for an RTOS
does not apply. embassy is pure Rust, `no_std`, and matches the firmware's event-driven shape.
Marked **"for now"** — reversible. embassy boots with the least arch glue (fastest to first
boot). If slow software-crypto fairness (the one real caveat, below) proves painful on hardware,
the escalation ladder is: **embassy (now) → RTIC 2 → FreeRTOS-rust (last resort)**.

- **RTIC 2** (preferred upgrade) — pure-Rust, preemptive *priority* scheduling built on the
  interrupt controller (not a kernel); its "hardware-task = ISR at priority, software-task =
  async executor" model IS this BMC's ISR→work pattern, and preemption fixes the crypto caveat
  WITHOUT a C kernel. Its async software-task executors are embassy-style, so the async
  driver/HAL code ports over. **Cost:** no ARMv5/ARM9 backend exists — we would write one
  (ARMv5 core IRQ/context + the Gromit-XE 2-bank controller, docs/12). HW supports it: per-source
  enable `+0x14` = source-masking critical sections (guaranteed); `+0x100[32]` config may give
  true HW priority. Novel/unproven but feasible.
- **FreeRTOS-rust** (last resort) — preemptive, the jkovacic ARM926 C port exists (least R&D),
  but puts a C kernel in an otherwise pure-Rust image.

The HAL/services sit above the executor, and RTIC 2 reuses embassy-style async, so a later swap
is contained.

**Carry-forward risk:** a long-running `rustls` op (RSA/ECDHE, tens–hundreds ms on 250 MHz
ARMv5) blocks the cooperative executor. Mitigations to design in from the start: keep TLS
handshakes off the hot path, prefer ECDHE+ChaCha20, session resumption, and if needed run
crypto on a timer-driven preemptive worker. Safety (watchdog pet, thermal fail-safe) goes in a
timer ISR above the executor regardless.

## The OS: Green Hills INTEGRITY (recap)

Hard-RT separation-kernel RTOS, **INTEGRITY i10.0**, GHS toolchain
(`ghsint1002`), build `intbuild112210` (22 Nov 2010). Evidence in docs/02 +
binary paths (`rtos-i10.0/...`, `.libINTEGRITY.so`, `interruptcontroller.c`).
Native model: AddressSpaces (memory-isolated partitions), Tasks, Connections +
MessageQueues (IPC), IODevice driver objects, an optional POSIX veneer. The
*capability* is hard-RT; the question is whether iLO's code *uses* it as such.

## How the firmware actually uses the scheduler → it DOESN'T use hard-RT

Task priorities are baked verbatim into the `CommonCreateTask(prio, entry, stack,
name, …)` debug strings, so the real scheduling intent is directly readable:

| Priority | Tasks | Meaning |
|---------:|-------|---------|
| **1** | `IdleTask` | system idle (lowest) |
| **120** | `dvc_main` (video engine), `report_diags` | **explicitly below** everything — video is best-effort/droppable |
| **127** | all 24 others (health, uid, IRC/p84, clipboard, events, net, clock, …) | one uniform mid band |

(INTEGRITY: 0–255, higher = higher priority; 127 = mid.) So **every management
task runs at the SAME priority**; the only deliberate priority decision HP made is
"video is lowest, so KVM never starves management." That is a **soft-real-time,
best-effort** design — not priority-driven hard-RT.

Reinforcing it:
- **`SetTaskTimer(t, TIMESLICEINDEX, &inf)`** — time-slice quantum = **infinite**:
  tasks run until they block, not fixed hard slices.
- **`SetPriorityAndWeight(...)`** — among the equal-priority band, INTEGRITY does
  **weighted fair CPU sharing**, not priority preemption. This is how the box stayed
  responsive while a task did slow software crypto (its one scheduling challenge).
- Tasks are **event-driven**: each blocks on a MessageQueue/Semaphore, wakes on an
  event, works, blocks again (`SynchronousReceive`, `wait_event`, `wait_clipboard`,
  `poll_health`). This is exactly the shape of `async`/`await`.

## Where the real-time actually lives: ISRs + hardware engines (not tasks)

All wire-level timing is **off-loaded from the task scheduler**:
- **Hardware engines do the protocol timing** — I2C ×8, PECI, MAC DMA, SPI-NOR,
  crypto(none/SW), virtual-UHCI. Tasks just kick an engine and poll a *done* flag
  with generous timeouts (`"I2C Bus Timeout"`, `"PECI timeout"`, `"Write operation
  NOT done"`). **No bit-bang, no `udelay`, no tight/busy-wait loops** exist in the
  app image (grep: none).
- **ISRs handle the latency-critical ack**, then hand off to a task via a queue —
  `kernel_type2.bin`: `interruptcontroller.c`, `.vector`/`.ptvectorremap`,
  **`MessageQueue Interrupt-Send Notifier`** (the ISR→task notify primitive),
  `CannotRaiseTaskInterruptPriorityLevelBelowCurrentLevel`. Classic split: ISR = fast
  + deterministic; task = does the work whenever scheduled.

**Key consequence:** interrupt handlers preempt **in every execution model** (bare
metal, async, or RTOS). The hard-timing half of iLO 3 is in ISRs, which the choice of
task scheduler does not affect.

## Per-subsystem real-time dependency (the actual audit)

| Subsystem | Timing need | Who meets it | Class |
|-----------|-------------|--------------|-------|
| I2C sensors / LM75 / PSU (panda) | ms; engine-timed | HW I2C engine + done-IRQ | **soft** |
| PECI CPU temp | µs on the wire | HW PECI engine (docs/26) + IRQ60 | **soft** (task reads result) |
| Fan tach / PWM scan-chain | ms sample | CPLD scan-chain + CPLD latch | **soft** |
| Fan FAIL-SAFE (force-MAX on sensor fault) | ~1 s | periodic poll + actuate | **soft-safety** |
| Thermal-critical → OS shutdown | ~1 s | event → power action | **soft-safety** |
| Power button hold (graceful vs force) | seconds (coarse) | GPIO + counter timing | **soft** |
| Host CHIF / ROM-CHIF mailbox | ms; doorbell | shared-mem + doorbell IRQ | **soft** (host waits) |
| IPMI / KCS | KCS timeouts = seconds | state machine + IRQ | **soft** |
| Virtual USB (UHCI) | NOT real USB wire timing — BMC walks the host frame-list at its own pace; host sees a slow device | CHIF window + walker task | **soft** |
| KVM video capture (DVC) | frame-rate = UX, droppable | lowest-priority `dvc_main` (120) | **soft / best-effort** |
| MAC RX/TX | line-rate bursts | HW DMA rings + IRQ 16/17 | **ISR** (preempts anyway) |
| Watchdogs (IPMI WDT, Task WDT, ASR) | 100 ms–seconds | timer + `bmc_wdt_alarm_handler` | **soft-safety** |
| SW crypto during TLS | none hard, but long-running (RSA/ECDHE tens–hundreds ms) | weighted time-slice | **fairness** |

**Nothing is hard-real-time.** Every "timing" requirement is either (a) owned by a
hardware engine, (b) owned by an ISR (preempts in any model), or (c) coarse
(ms–seconds) soft/best-effort. There are **no deadlines, no periodic hard tasks, no
µs task paths, no bit-bang**. The firmware is an **event-driven soft-RT system**.

## Verdict — a hard-real-time RTOS is NOT required

The premise "it's an RTOS, so the rewrite needs an RTOS" is **false for determinism**.
iLO 3 does not use INTEGRITY's hard-RT guarantees — flat priorities, infinite
time-slice, event-driven tasks, HW/ISR-owned timing. What the rewrite genuinely needs
from its execution model is only:

1. **ISRs that preempt** — ack engines/timer/MAC/CHIF, notify tasks. *Every* model
   (bare-metal, async, RTOS) provides this. Not a differentiator.
2. **Event-driven tasks that block/wake on I/O** — async tasks **or** RTOS tasks. ✔ both.
3. **Best-effort, droppable video at lowest priority** — a yield point / low-priority
   task. ✔ both.
4. **Safety that can't be starved** (watchdog pet, thermal fail-safe) — put in a timer
   ISR / high-priority context, independent of the main scheduler. ✔ both (trivially).
5. **Fair CPU sharing so slow software crypto doesn't starve management** — the ONE
   place the models differ. The original solved it with **weighted time-slicing**
   (preemptive). A **cooperative async** executor cannot slice a *running* crypto
   primitive (no yield inside `rustls`); a **preemptive** scheduler can.

### So the decision reduces to ONE axis — crypto responsiveness, not real-time

- **Hard-RT RTOS (VxWorks/INTEGRITY-class determinism): not needed.** Remove it from
  the requirements entirely.
- **embassy (pure-Rust cooperative async):** satisfies 1–4 cleanly; for 5, a long
  software-crypto op blocks the executor — mitigate by running TLS/crypto on a
  **separate preemptive context** (a timer-driven worker, or accept "sequential +
  slow during handshake" — iLO 3 itself was already slow here).
- **FreeRTOS-rust (preemptive, ARM926 port):** satisfies 5 natively via preemptive
  weighted/time-sliced scheduling — the closest match to how the original behaves —
  plus 1–4; cost = a small C kernel in the build.
- **Minimal custom preemptive scheduler:** also viable — a tiny tick-preemptive
  round-robin is enough (we don't need priorities), but it's reinventing what FreeRTOS
  already ships + validates on ARM926.

**Bottom line:** No hard-RT requirement exists. Pick the execution model on
*developer ergonomics + how gracefully it handles slow software crypto*, not on
real-time determinism. If "stay responsive during TLS exactly like the original" is a
goal → a preemptive scheduler (FreeRTOS-rust) is the faithful, low-effort match. If
"pure Rust, accept sequential crypto" is acceptable → embassy async is simpler and
cleaner. Both are correct; neither needs an RTOS *for real-time*.
