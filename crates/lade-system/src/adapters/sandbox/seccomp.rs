const LD_W_ABS: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JGE_K: u16 = 0x35;
const JSET_K: u16 = 0x45;
const RET_K: u16 = 0x06;

const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const EPERM: u32 = 1;
const ENOSYS: u32 = 38;

const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const fn arg_low(index: u32) -> u32 {
    16 + 8 * index
}

const CLONE_NAMESPACES: u32 = 0x7e02_0000;
const CLONE_NEWTIME: u32 = 0x80;
const PER_LINUX: u32 = 0;
const PER_QUERY: u32 = 0xffff_ffff;
const TIOCSTI: u32 = 0x5412;
const TIOCLINUX: u32 = 0x541c;

struct Arch {
    audit: u32,
    x32_bit: Option<u32>,
    clone: u32,
    clone3: u32,
    unshare: u32,
    personality: u32,
    ioctl: u32,
    denied: &'static [(&'static str, u32)],
}

#[cfg(target_arch = "x86_64")]
const ARCH: Option<Arch> = Some(Arch {
    audit: 0xc000_003e,
    x32_bit: Some(0x4000_0000),
    clone: 56,
    clone3: 435,
    unshare: 272,
    personality: 135,
    ioctl: 16,
    denied: &[
        ("pivot_root", 155),
        ("mount", 165),
        ("umount2", 166),
        ("swapon", 167),
        ("swapoff", 168),
        ("reboot", 169),
        ("init_module", 175),
        ("delete_module", 176),
        ("kexec_load", 246),
        ("add_key", 248),
        ("request_key", 249),
        ("keyctl", 250),
        ("perf_event_open", 298),
        ("open_by_handle_at", 304),
        ("setns", 308),
        ("finit_module", 313),
        ("kexec_file_load", 320),
        ("bpf", 321),
        ("userfaultfd", 323),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
    ],
});

#[cfg(target_arch = "aarch64")]
const ARCH: Option<Arch> = Some(Arch {
    audit: 0xc000_00b7,
    x32_bit: None,
    clone: 220,
    clone3: 435,
    unshare: 97,
    personality: 92,
    ioctl: 29,
    denied: &[
        ("umount2", 39),
        ("mount", 40),
        ("pivot_root", 41),
        ("kexec_load", 104),
        ("init_module", 105),
        ("delete_module", 106),
        ("reboot", 142),
        ("add_key", 217),
        ("request_key", 218),
        ("keyctl", 219),
        ("swapon", 224),
        ("swapoff", 225),
        ("perf_event_open", 241),
        ("open_by_handle_at", 265),
        ("setns", 268),
        ("finit_module", 273),
        ("bpf", 280),
        ("userfaultfd", 282),
        ("kexec_file_load", 294),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
    ],
});

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const ARCH: Option<Arch> = None;

#[must_use]
pub fn denied() -> Vec<&'static str> {
    ARCH.as_ref()
        .map(|arch| arch.denied.iter().map(|(name, _)| *name).collect())
        .unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Next,
    Allow,
    Eperm,
    Enosys,
    Kill,
    Label(usize),
}

struct Insn {
    code: u16,
    k: u32,
    jt: Target,
    jf: Target,
}

#[derive(Default)]
struct Asm {
    insns: Vec<Insn>,
    labels: Vec<Option<usize>>,
}

impl Asm {
    fn op(&mut self, code: u16, k: u32, jt: Target, jf: Target) {
        self.insns.push(Insn { code, k, jt, jf });
    }

    fn load(&mut self, offset: u32) {
        self.op(LD_W_ABS, offset, Target::Next, Target::Next);
    }

    fn jeq(&mut self, k: u32, target: Target) {
        self.op(JEQ_K, k, target, Target::Next);
    }

    fn ret(&mut self, value: u32) {
        self.op(RET_K, value, Target::Next, Target::Next);
    }

    fn label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn place(&mut self, label: usize) {
        self.labels[label] = Some(self.insns.len());
    }

    /// Appends the shared return instructions and resolves every jump target to a relative
    /// offset. `None` when a label was never placed or a jump is farther than the 8 bits a
    /// classic BPF conditional jump encodes.
    fn finish(mut self) -> Option<Vec<u8>> {
        let base = self.insns.len();
        for value in [
            RET_ALLOW,
            RET_ERRNO | EPERM,
            RET_ERRNO | ENOSYS,
            RET_KILL_PROCESS,
        ] {
            self.ret(value);
        }
        let resolve = |target: Target, at: usize| -> Option<u8> {
            let to = match target {
                Target::Next => at + 1,
                Target::Label(label) => (*self.labels.get(label)?)?,
                Target::Allow => base,
                Target::Eperm => base + 1,
                Target::Enosys => base + 2,
                Target::Kill => base + 3,
            };
            u8::try_from(to.checked_sub(at + 1)?).ok()
        };
        let mut out = Vec::with_capacity(self.insns.len() * 8);
        for (at, insn) in self.insns.iter().enumerate() {
            out.extend_from_slice(&insn.code.to_ne_bytes());
            out.push(resolve(insn.jt, at)?);
            out.push(resolve(insn.jf, at)?);
            out.extend_from_slice(&insn.k.to_ne_bytes());
        }
        Some(out)
    }
}

#[must_use]
pub fn program() -> Option<Vec<u8>> {
    let arch = ARCH.as_ref()?;
    if cfg!(target_endian = "big") {
        return None;
    }
    let mut asm = Asm::default();
    let (personality, ioctl, clone, unshare) = (asm.label(), asm.label(), asm.label(), asm.label());
    asm.load(OFFSET_ARCH);
    asm.op(JEQ_K, arch.audit, Target::Next, Target::Kill);
    asm.load(OFFSET_NR);
    if let Some(bit) = arch.x32_bit {
        asm.op(JGE_K, bit, Target::Eperm, Target::Next);
    }
    for (_, nr) in arch.denied {
        asm.jeq(*nr, Target::Eperm);
    }
    asm.jeq(arch.clone3, Target::Enosys);
    asm.jeq(arch.personality, Target::Label(personality));
    asm.jeq(arch.ioctl, Target::Label(ioctl));
    asm.jeq(arch.clone, Target::Label(clone));
    asm.jeq(arch.unshare, Target::Label(unshare));
    asm.ret(RET_ALLOW);

    asm.place(personality);
    asm.load(arg_low(0));
    asm.jeq(PER_LINUX, Target::Allow);
    asm.jeq(PER_QUERY, Target::Allow);
    asm.ret(RET_ERRNO | EPERM);

    asm.place(ioctl);
    asm.load(arg_low(1));
    asm.jeq(TIOCSTI, Target::Eperm);
    asm.jeq(TIOCLINUX, Target::Eperm);
    asm.ret(RET_ALLOW);

    asm.place(clone);
    asm.load(arg_low(0));
    asm.op(JSET_K, CLONE_NAMESPACES, Target::Eperm, Target::Allow);

    asm.place(unshare);
    asm.load(arg_low(0));
    asm.op(
        JSET_K,
        CLONE_NAMESPACES | CLONE_NEWTIME,
        Target::Eperm,
        Target::Allow,
    );
    asm.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Data {
        nr: u32,
        arch: u32,
        args: [u64; 6],
    }

    fn run(program: &[u8], data: &Data) -> u32 {
        let insns: Vec<(u16, u8, u8, u32)> = program
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                (
                    u16::from_ne_bytes([c[0], c[1]]),
                    c[2],
                    c[3],
                    u32::from_ne_bytes([c[4], c[5], c[6], c[7]]),
                )
            })
            .collect();
        let word = |offset: u32| match offset {
            OFFSET_NR => data.nr,
            OFFSET_ARCH => data.arch,
            o => {
                let index = ((o - 16) / 8) as usize;
                let value = data.args[index];
                if (o - 16) % 8 == 0 {
                    u32::try_from(value & u64::from(u32::MAX)).expect("masked to the low word")
                } else {
                    (value >> 32) as u32
                }
            }
        };
        let (mut pc, mut a) = (0usize, 0u32);
        loop {
            let (code, jt, jf, k) = insns[pc];
            let jump = |taken: bool| pc + 1 + usize::from(if taken { jt } else { jf });
            pc = match code {
                LD_W_ABS => {
                    a = word(k);
                    pc + 1
                }
                JEQ_K => jump(a == k),
                JGE_K => jump(a >= k),
                JSET_K => jump(a & k != 0),
                RET_K => return k,
                other => panic!("unexpected opcode {other:#x}"),
            };
        }
    }

    fn call(arch: &Arch, nr: u32, args: [u64; 6]) -> u32 {
        run(
            &program().unwrap(),
            &Data {
                nr,
                arch: arch.audit,
                args,
            },
        )
    }

    #[test]
    fn denylist_and_argument_checks() {
        let Some(arch) = ARCH.as_ref() else { return };
        let eperm = RET_ERRNO | EPERM;
        for (_, nr) in arch.denied {
            assert_eq!(call(arch, *nr, [0; 6]), eperm);
        }
        assert_eq!(call(arch, 0, [0; 6]), RET_ALLOW);
        assert_eq!(call(arch, 63, [0; 6]), RET_ALLOW);
        assert_eq!(call(arch, arch.clone3, [0; 6]), RET_ERRNO | ENOSYS);
        assert_eq!(call(arch, arch.clone, [0x11, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(
            call(arch, arch.clone, [0x003d_0f00, 0, 0, 0, 0, 0]),
            RET_ALLOW
        );
        assert_eq!(call(arch, arch.clone, [0x1000_0011, 0, 0, 0, 0, 0]), eperm);
        assert_eq!(
            call(arch, arch.unshare, [0x0000_0400, 0, 0, 0, 0, 0]),
            RET_ALLOW
        );
        assert_eq!(
            call(arch, arch.unshare, [0x1000_0000, 0, 0, 0, 0, 0]),
            eperm
        );
        assert_eq!(call(arch, arch.unshare, [0x80, 0, 0, 0, 0, 0]), eperm);
        assert_eq!(call(arch, arch.personality, [0, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(
            call(arch, arch.personality, [0xffff_ffff, 0, 0, 0, 0, 0]),
            RET_ALLOW
        );
        assert_eq!(
            call(arch, arch.personality, [0x0040_0000, 0, 0, 0, 0, 0]),
            eperm
        );
        assert_eq!(call(arch, arch.ioctl, [0, 0x5412, 0, 0, 0, 0]), eperm);
        assert_eq!(
            call(arch, arch.ioctl, [0, 0x1_0000_5412, 0, 0, 0, 0]),
            eperm
        );
        assert_eq!(call(arch, arch.ioctl, [0, 0x541c, 0, 0, 0, 0]), eperm);
        assert_eq!(call(arch, arch.ioctl, [0, 0x5413, 0, 0, 0, 0]), RET_ALLOW);
        if let Some(bit) = arch.x32_bit {
            assert_eq!(call(arch, bit, [0; 6]), eperm);
        }
        let foreign = run(
            &program().unwrap(),
            &Data {
                nr: 0,
                arch: 0x4000_0003,
                args: [0; 6],
            },
        );
        assert_eq!(foreign, RET_KILL_PROCESS);
    }
}
