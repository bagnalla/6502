### Motivation
A simple CPU emulator generally looks something like this:

```cpp
while (1) {
    switch (code[pc++]) {
        case OP_ADC: {
            // execute add with carry
        }
        case OP_HLT: {
            // terminate execution
            break;
        }
        // cases for other opcodes ...
    }
}
```

Details may vary, like using a lookup table instead of a switch statement, but the key thing is that each instruction is implemented by a section of code that is a direct interpretation of its semantics, which makes it easy to write and understand.

But there's a big downside: the timing details are abstracted away. Each instruction executes all at once, unlike in real hardware where it proceeds through a series of states over multiple clock cycles.

This can be a challenge when integrating the CPU in a larger system in which it has timing-sensitive interactions with other components. For example, the Nintendo Entertainment System (NES) has a 6502 CPU and a graphics co-processor called the PPU that runs concurrently with the CPU but at three times its clock rate.

Using a CPU emulator like the one above, a NES emulator might look something like this (ignoring the APU and many other details):

```cpp
void nes() {
    while (1) {
        cpu_step(); // execute one instruction on cpu
        for (...) { // catch up ppu
            ppu_step();
        }
    }
}
```

When the CPU executes an instruction, it gets way out of sync with the PPU (by up to 21 PPU cycles), which then needs to fast-forward to "catch up". This can cause emulation accuracy problems that require complicated prediction/rewinding logic to deal with (you can read more about it [here](https://www.nesdev.org/wiki/Catch-up)).

The true solution to the catch-up problem is to avoid it altogether by implementing the CPU the right way in the first place, i.e., such that it can be stepped one cycle at a time. Then the NES emulator can look like:

```cpp
void nes() {
    int cycle = 0;
    while (1) {
        if (cycle++ % 3 == 0) {
            cpu_step(); // one cycle on cpu
        }
        ppu_step(); // one cycle on ppu
    }
}
```

Achieving this boils down to implementing the CPU as a state machine where the state transitions correspond to clock cycles [^1]. There are various ways of doing that, of course, including translation to microcode. But they tend to be rather complicated -- not like a simple ISA interpreter.

This project shows how to get the best of both worlds using coroutines in Rust. We define the emulation semantics of a 6502 CPU in a nice interpreter form, and Rust compiles it for us to a state machine that can be run in lockstep with the PPU.

### 6502 implementation

The Cpu state is a struct with fields for all the registers, and an 8-bit data bus that all reads and writes to memory go through.

```rust
pub struct Cpu {
    pub pc: u16, // program counter
    pub sp: u8,  // stack pointer
    pub a: u8,   // accumulator
    // ... more 8-bit registers, including status

    pub io_bus: Arc<AtomicU8>, // data bus for I/O
}
```

When the CPU yields control, it raises an IO event which can be a read or a write (usually the 6502 performs a read or write on every cycle, even when not necessary).

```rust
pub enum IO {
    Read,
    Write,
}
pub type CpuEvent = (u16, Option<IO>);
```

And the behavior of the CPU is defined by its `run` method, where the instruction dispatch loop runs in a coroutine. The return type of `run` is a bit of a mouthful, but it just means that it's a coroutine that yields CpuEvents and returns a String if/when it terminates.

```rust
impl Cpu {
   pub fn run(&mut self) -> Box<dyn Coroutine<Yield = CpuEvent, Return = String> + Unpin + '_> {
        let go = #[coroutine] move || {
            loop {
                let instr_code = next_pc!();
                let instr = Self::decode(instr_code);

                // compute fetch_addr, etc.

                match instr.opcode {
                    Nop => match instr.mode {
                        AddrMode::Imp => (),
                        _ => {
                            let _ = fetch_oops!();
                        }
                    }
                    And => {
                        let val = fetch_oops!();
                        self.a &= val;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0x80 != 0);
                    }
                    Sta => match instr.mode {
                        Abx | Aby | Izy => {
                            let _ = fetch!(fetch_addr);
                            write!(corrected_addr, self.a)
                        }
                        _ => write!(fetch_addr, self.a),
                    }
                    Pha => {
                        push!(self.a),
                    }
                    Hlt => {
                        return "HALT".into()
                    }
                    // other opcodes ...
                }
            }
        };
        Box::new(go)
    }
}
```

The implementation of each instruction is a straightforward transliteration of its cycle-by-cycle breakdown described [in this reference](https://www.nesdev.org/6502_cpu.txt).

### I/O Macros

`next_pc!`, `fetch_oops!`, and `write!` are macros for I/O events. The most basic ones are `fetch!` and `write!`:

```rust
macro_rules! fetch {
    ( $adr:expr ) => {
        {
            yield ($adr, Some(IO::Read));
            self.io_bus.load(Ordering::Relaxed)
        }
    };
}

macro_rules! write {
    ( $adr:expr, $val:expr ) => {
        {
            self.io_bus.store($val, Ordering::Relaxed);
            yield ($adr, Some(IO::Write))
        }
    };
}

// More macros...
```

See the [source](src/lib.rs#L168) for the others.

### Driving the CPU and PPU together

Here's example code for running the CPU and PPU together in lockstep. An actual implementation in a NES emulator is [here](https://github.com/bagnalla/nes/blob/main/nes/src/lib.rs). It isn't finished -- maybe someday I'll get back to it. IIRC, it can load and run a test rom but no real games yet.

```rust
let mut cpu = Cpu::new();
let cpu_bus = cpu.io_bus.clone();
let mut cpu_process = cpu.run();

let mut ppu = Ppu::new();
let ppu_regs = ppu.regs.clone();
let mut ppu_process = ppu.run();

loop {
    // Step PPU
    match Pin::new(&mut ppu_process).resume(()) {
        CoroutineState::Yielded(ppu_event) =>
            self.handle_ppu_event(&mut cart ppu_event),
        CoroutineState::Complete(msg) => return msg
    }

    // Step CPU every third cycle
    if cycle_counter % 3 == 0 {
        match Pin::new(&mut cpu_process).resume(()) {
            CoroutineState::Yielded(cpu_event) =>
                self.handle_cpu_event(&mut cart, &cpu_bus, &ppu_regs, cpu_event),
            CoroutineState::Complete(msg) => return msg
        }
    }
}
```

`self.handle_cpu_event` mediates access to RAM and memory-mapped registers on the PPU. E.g., when the CPU requests a read at a given memory address, `handle_cpu_event` loads the byte at that address onto the CPU's data bus, where it will be read and used by the CPU when it resumes on the next cycle.

### Performance

I haven't done an extensive performance evaluation, but at one point I clocked it at ~470 MHz on my computer with an i9-14900K processor. The NES needs it to run at 1.79 MHz.

### Tests

- **Harte tests:** [test/harte/README.md](test/harte/README.md) - 10,000 tests per opcode
- **Klaus functional tests:** [test/klaus/README.md](test/klaus/README.md)

Run with:
```bash
cargo test --release
```
The Harte tests take a few minutes.

### Disclaimer

**["Coroutines are an extra-unstable feature in the compiler right now."](https://doc.rust-lang.org/beta/unstable-book/language-features/coroutines.html)**

This project must be built with Rust nightly and could be broken by future changes to the Rust compiler. At the time of writing, it works with Rust version 1.93.0-nightly (278a90913 2025-10-28).

### Related work

I'm not the first to think of using coroutines for emulation. For example, the [bsnes](https://github.com/bsnes-emu/bsnes) emulator apparently is based on similar ideas, using the [libco](https://github.com/higan-emu/libco) coroutine library for C. I'm sure there are others!

[^1]: There's a common alternative to the state machine approach: inserting calls to the PPU's step function in the CPU at all the necessary points. But this still makes the CPU more complicated, and it is less modular since it's tightly coupled with the PPU.
