#![feature(result_contains_err)]

use argh::FromArgs;
use bbc::machine::{Direction, Machine};
use color_eyre::eyre::Result;
use owo_colors::OwoColorize;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;
use termion::{event::Key, input::TermRead, raw::IntoRawMode, screen::IntoAlternateScreen};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Err {
    Halt,
    StepLimit,
    Interesting,
    UnknownTransition,
    Unreachable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Item {
    /// left: `011 01`, right: `110 10`
    D,
    /// 011
    P,
    /// 0: `011 0111 011`, 1: `011 0 01`, 2: `01111 011`, 3: `01 01`
    C(u8),
    /// `(011 011)^n`
    X(usize),
    /// 1-run-length encoding; `L(2332)` == `011 0111 0111 011`
    L(u16),
    /// 0/a: `2 x^7640 D x^10344 ``
    /// 1/b: `D x^72142 D x^3076 D x^1538 D x^300 D x^30826`
    /// 2/c: `1D x^72141 1D x^3075 1D x^1537 1D x^299 1D x^30825`
    E {
        block: u8,
        exp: usize,
    },
    Unreachable,
}

type Tape = Vec<Item>;

type RefBlocks<'a> = &'a [&'a [Item]];

#[derive(Clone, Debug)]
struct Configuration {
    // tape: [Tape; 2],
    // head: Head,
    ltape: Tape,
    rtape: Tape,
    /// `<C 10` | `A>`
    dir: Direction,
    sim_step: usize,
}

// >  xCC      ->  {2332}    >
//   {2332} <  ->  {2301}  x >
//   {2301} <  ->  {252}     >
//   {252}  <  ->    PDx     >
//
// x C1 C  <   ->  x {432}   >
// x {432} <   ->  x {401} x >
// x {401} <   ->  x {62}    >
// x {62}  <   ->  x {31}  x >
// x {31}  <   ->    P C1 D  >

impl Configuration {
    pub fn new() -> Configuration {
        // $ cargo run --release --bin on2 5 0
        // ...
        // `28:  11001 <C 1011` == `C1 < P`

        Configuration {
            // tape: [Vec::new(), Vec::new()],
            // head: Head { state: 0, direction: Direction::Right },
            ltape: vec![Item::C(1)],
            rtape: vec![Item::P],
            dir: Direction::Right,
            sim_step: 0,
        }
    }

    fn run(&mut self, _machine: &Machine, _blocks: RefBlocks, cfg: Config) -> Result<(), Err> {
        #[inline(always)]
        fn push_or_merge_x(tape: &mut Tape, new_exp: usize) {
            if let Some(Item::X(exp)) = tape.last_mut() {
                *exp = exp.checked_add(new_exp).unwrap();
            } else {
                tape.push(Item::X(new_exp));
            }
        }
        macro_rules! pop_x_truncate {
            ($tape:tt, $exp:tt) => {
                *$exp -= 1;
                if *$exp == 0 {
                    self.$tape.pop();
                }
            };
            ($tape:tt, $exp:tt, $extra:tt) => {
                *$exp -= 1;
                let remove = if *$exp == 0 { $extra + 1 } else { $extra };
                self.$tape.truncate(self.$tape.len() - remove);
            };
        }

        use Direction::*;
        while self.sim_step < cfg.sim_step_limit {
            match (self.dir, self.ltape.as_mut_slice(), self.rtape.as_mut_slice()) {
                // NEW `end < 3x` -> `1 > DP` // $ cargo run --release --bin on2 4 0 --conf "! 00 <C 10 1010 110 110 !" ... 15:   ! a^1 001 A> a^1 10110 !
                (Left, [], [.., Item::X(exp), Item::C(3)]) => {
                    pop_x_truncate!(rtape, exp, 1);
                    self.rtape.push(Item::P);
                    self.rtape.push(Item::D);
                    self.ltape.push(Item::C(1));
                    self.dir = Right;
                }
                // NEW `x > end` -> ` < 3xP` // $  cargo run --release --bin on2 8 0 --conf "! 011011 A> 00000000 !"
                (Right, [.., Item::X(exp)], []) => {
                    pop_x_truncate!(ltape, exp);
                    self.rtape.push(Item::P);
                    self.rtape.push(Item::X(1));
                    self.rtape.push(Item::C(3));
                    self.dir = Left;
                }
                // NEW `D > end` -> `< x` // $ cargo run --release --bin on2 4 0 --conf "! 011 01 A> 000 !" ... 6:   <C 10 a^2 !
                (Right, [.., Item::D], []) => {
                    self.ltape.pop();
                    self.rtape.push(Item::X(1)); // || test Item::P + Item::P
                    self.dir = Left;
                }

                // `> D33` -> `P0 >` // $ cargo run --release --bin on2 8 0 --conf "! A> 11010 1010 1010 !"
                (Right, _, [.., Item::C(3), Item::C(3), Item::D]) => {
                    self.rtape.truncate(self.rtape.len() - 3);
                    self.ltape.push(Item::P);
                    self.ltape.push(Item::C(0));
                }
                // `> D3` -> `xP >`
                (Right, _, [.., Item::C(3), Item::D]) => {
                    self.rtape.truncate(self.rtape.len() - 2);
                    push_or_merge_x(&mut self.ltape, 1);
                    self.ltape.push(Item::P);
                }

                // `(x | D | P | 3) < ` -> `< (x | D | P | 3)`
                (Left, [.., Item::X(_) | Item::D | Item::P | Item::C(3)], _) => {
                    let item = self.ltape.pop().unwrap();
                    self.rtape.push(item);
                }
                // needs to be after previous case (c != 3)
                // `0 <` -> `1x >` | `1 < ` -> `2 >` | `2 <` -> `3x >`
                (Left, [.., Item::C(c)], _) => {
                    *c += 1;
                    if *c != 2 {
                        self.ltape.push(Item::X(1));
                    }
                    self.dir = Right;
                }

                // CHANGED `x > 3` -> `0 >` // from `> x^n 3` -> `x^(n-1) 0 >`
                (Right, [.., Item::X(exp)], [.., Item::C(3)]) => {
                    let test_a = *exp == 10345; // --conf "2 x^7640 D x^10344 2 x^7640 D x^10344 1 x^7640 D x^10345 3 x^7639 D x^10347 3 < ! "
                    pop_x_truncate!(ltape, exp);
                    if test_a && self.ltape.ends_with(&[Item::C(2), Item::X(7640), Item::D, Item::X(10344)]) {
                        self.ltape.truncate(self.ltape.len() - 4);
                        if let Some(Item::E { block: 0, exp }) = self.ltape.last_mut() {
                            *exp = exp.checked_add(1).unwrap();
                        } else {
                            self.ltape.push(Item::E { block: 0, exp: 1 })
                        }
                    }
                    self.ltape.push(Item::C(0));
                    self.rtape.pop();
                }

                // `0 > 3` (== `> x33`) -> `L(2332) >` // $ cargo run --release --bin on2 8 0 --conf "! 011 0111 011 A> 1010 !"
                (Right, [.., Item::C(0)], [.., Item::C(3)]) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(2332));
                    self.rtape.pop();
                }
                // `L(2332) <` -> `L(2301) x >` // $ cargo run --release --bin on2 8 0 --conf "! 01101110111 a^1 <C 10 !"
                (Left, [.., Item::L(2332)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(2301));
                    self.ltape.push(Item::X(1));
                    self.dir = Right;
                }
                // `L(2301) <` -> `L(252) >` // $ cargo run --release --bin on2 8 0 --conf "! 0110111001 <C 10 !"
                (Left, [.., Item::L(2301)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(252));
                    self.dir = Right;
                }
                // `L(252) <` -> `PDx >` // $ cargo run --release --bin on2 8 0 --conf "! 011011111 a^1  <C 10 !"
                (Left, [.., Item::L(252)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::P);
                    self.ltape.push(Item::D);
                    self.ltape.push(Item::X(1));
                    self.dir = Right;
                }
                // `> PD3x` -> `L(2301) D > P` // $ cargo run --release --bin on2 5 0 --conf "! A> 110 11010 1010 110110 !" ... 31:   !  a^2 1001 a^1 01 A> 110 !
                (Right, _, [.., Item::X(exp), Item::C(3), Item::D, Item::P]) => {
                    pop_x_truncate!(rtape, exp, 3);
                    self.rtape.push(Item::P);
                    self.ltape.push(Item::L(2301));
                    self.ltape.push(Item::D);
                }
                // `> PDDx` -> `21D > ` // $ cargo run --release --bin on2 8 0 --conf "! A> 110 11010 11010 110110 !" ... 63:   !  a^1 11 a^2 001 a^1 01 A>  !
                (Right, _, [.., Item::X(exp), Item::D, Item::D, Item::P]) => {
                    pop_x_truncate!(rtape, exp, 3);
                    self.ltape.push(Item::C(2));
                    self.ltape.push(Item::C(1));
                    self.ltape.push(Item::D);
                }
                // `2 > 3` (== `13 <`) -> `L(432) >` // $ cargo run --release --bin on2 8 0 --conf "! 011 0111 011 A> 1010 !"
                (Right, [.., Item::C(2)], [.., Item::C(3)]) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(432));
                    self.rtape.pop();
                }
                // `L(432) <` -> `L(401) x >` // $ cargo run --release --bin on2 8 0 --conf "! 011110111 a^1 <C 10 !"
                (Left, [.., Item::L(432)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(401));
                    self.ltape.push(Item::X(1));
                    self.dir = Right;
                }
                // `L(401) <` -> `L(62) >` // $ cargo run --release --bin on2 8 0 --conf "! 01111001 <C 10 !"
                (Left, [.., Item::L(401)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(62));
                    self.dir = Right;
                }
                // `L(62) <` -> `L(31) x >` // $ cargo run --release --bin on2 8 0 --conf "! 0111111 a^1 <C 10 !"
                (Left, [.., Item::L(62)], _) => {
                    self.ltape.pop();
                    self.ltape.push(Item::L(31));
                    self.ltape.push(Item::X(1));
                    self.dir = Right;
                }
                // `x L(31) <` -> `P1D >` // $ cargo run --release --bin on2 8 0 --conf "! 011011 011101 <C 10 !"
                (Left, [.., Item::X(exp), Item::L(31)], _) => {
                    pop_x_truncate!(ltape, exp, 1);
                    self.ltape.push(Item::P);
                    self.ltape.push(Item::C(1));
                    self.ltape.push(Item::D);
                    self.dir = Right;
                }

                // `> P x^n` -> `x^n > P`
                (Right, _, [.., Item::X(exp), Item::P]) => {
                    push_or_merge_x(&mut self.ltape, *exp);
                    self.rtape.truncate(self.rtape.len() - 2);
                    self.rtape.push(Item::P)
                }
                // `> PDP` -> `1D >`
                (Right, _, [.., Item::P, Item::D, Item::P]) => {
                    self.rtape.truncate(self.rtape.len() - 3);
                    self.ltape.push(Item::C(1));
                    self.ltape.push(Item::D)
                }
                // `> PDx` -> `1D > P`
                (Right, _, [.., Item::X(exp), Item::D, Item::P]) => {
                    pop_x_truncate!(rtape, exp, 2);
                    self.rtape.push(Item::P);
                    self.ltape.push(Item::C(1));
                    self.ltape.push(Item::D);
                }
                // `> P3x` -> `< PDP`
                (Right, _, [.., Item::X(exp), Item::C(3), Item::P]) => {
                    pop_x_truncate!(rtape, exp, 2);
                    self.rtape.push(Item::P);
                    self.rtape.push(Item::D);
                    self.rtape.push(Item::P);
                    self.dir = Left;
                }
                // `> P end` -> `< P`
                (Right, _, [Item::P]) => {
                    self.dir = Left;
                }
                // CHANGED `> PP` -> `x >` // from `> PP end`
                (Right, _, [.., Item::P, Item::P]) => {
                    self.rtape.truncate(self.rtape.len() - 2);
                    push_or_merge_x(&mut self.ltape, 1);
                }
                // `> D` -> `D >`
                (Right, _, [.., Item::D]) => {
                    self.rtape.pop();
                    self.ltape.push(Item::D);
                }
                // `> x` -> `x >`
                (Right, _, [.., Item::X(exp)]) => {
                    let test_b = *exp == 30826; // --conf "2 > D x^598979953 PDP x^72142 D x^3076 D x^1538 D x^300 D x^30826 D x^42804942 D x^213427271 3 x^670661487 P"
                    push_or_merge_x(&mut self.ltape, *exp);
                    use Item::*;
                    if test_b && self.ltape.ends_with(&[D, X(72142), D, X(3076), D, X(1538), D, X(300), D, X(30826)]) {
                        self.ltape.truncate(self.ltape.len() - 10);
                        if let Some(Item::E { block: 1, exp }) = self.ltape.last_mut() {
                            *exp = exp.checked_add(1).unwrap();
                        } else {
                            self.ltape.push(Item::E { block: 1, exp: 1 })
                        }
                        // return Err(Err::Interesting);
                    }
                    self.rtape.pop();
                }
                // `> b` -> `b >`
                (Right, _, [.., Item::E { block: 1, exp: move_exp }]) => {
                    if let Some(Item::E { block: 1, exp }) = self.ltape.last_mut() {
                        *exp = exp.checked_add(*move_exp).unwrap();
                    } else {
                        self.ltape.push(Item::E { block: 1, exp: *move_exp })
                    }
                    self.rtape.pop();
                }
                // `b < ` -> `< b`
                (Left, [.., Item::E { block: 1, exp: move_exp }], _) => {
                    self.rtape.push(Item::E { block: 1, exp: *move_exp });
                    self.ltape.pop();
                }
                // `c^n < ` -> `c^(n-1) expanded-c <`
                (Left, [.., Item::E { block: 2, exp }], _) => {
                    *exp -= 1;
                    if *exp == 0 {
                        self.ltape.pop();
                    }
                    use Item::*;
                    let e = [C(1), D, X(72141), C(1), D, X(3075), C(1), D, X(1537), C(1), D, X(299), C(1), D, X(30825)];
                    self.ltape.extend_from_slice(&e); // NUDO: use extend() if Items gets bigger / allocates
                }
                (Left, [.., Item::Unreachable], _) | (Right, _, [.., Item::Unreachable]) => {
                    return Err(Err::Unreachable);
                }
                // `> P b` -> c > P // --conf "! > P   D x^72142 D x^3076 D x^1538 D x^300 D x^30826   D !"
                (Right, _, [.., Item::E { block: 1, exp: move_exp }, Item::P]) => {
                    // -> "! 1D x^72141 1D x^3075 1D x^1537 1D x^299 1D x^30825  > P !"
                    self.ltape.push(Item::E { block: 2, exp: *move_exp });
                    self.rtape.truncate(self.rtape.len() - 2);
                }

                _ => return Err(Err::UnknownTransition),
            }

            self.sim_step += 1;
            if self.sim_step & ((1 << cfg.print_mod) - 1) == 0 {
                println!("{self}");
            }
        }
        return Err(Err::StepLimit);
    }
}

// // NEW `x > end` -> `1 < P` // $ cargo run --release --bin on2 4 0 --conf "! 011011 A> 0000 !"  ... 15:   ! 011001 <C 1011 !
// (Right, [.., Item::X(exp)], []) => {
//     pop_truncate_x!(ltape, exp);
//     self.ltape.push(Item::C(1));
//     self.rtape.push(Item::P);
//     self.dir = Left;
// }
// // `> PP end` -> `< 3xP` // $ cargo run --release --bin on2 8 0 --conf "! A> 11011000000000 !" ...  63:   ! <C 10 1010 a^2 11 !
// (Right, _, [Item::P, Item::P]) => {
//     self.rtape.pop();
//     self.rtape.push(Item::X(1));
//     self.rtape.push(Item::C(3));
//     self.dir = Left;
// }

impl fmt::Display for Configuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn fmt_symbol(item: &Item, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *item {
                Item::D => write!(f, "D"),
                Item::P => write!(f, "P"),
                Item::C(s) => write!(f, "{}", s.italic().bold()),
                Item::X(exp) => {
                    if exp > 1_000_000_000 {
                        write!(f, " x^{} ", exp.bright_white())
                    } else {
                        write!(f, " x^{} ", exp)
                    }
                }
                Item::L(r) => write!(f, " L({r}) "),
                Item::E { block, exp } => write!(f, " {}^{} ", ((block + b'a') as char).yellow().bold(), exp),
                Item::Unreachable => write!(f, " {} ", '!'.bright_red()),
            }
        }

        write!(f, "{}:  ", self.sim_step.bright_white())?;
        self.ltape.iter().try_for_each(|item| fmt_symbol(item, f))?;
        write!(f, " {} ", if self.dir == Direction::Left { '<' } else { '>' }.bright_green().bold())?;
        self.rtape.iter().rev().try_for_each(|item| fmt_symbol(item, f))
    }
}

fn raw_parse(s: &str) -> Result<(Configuration, Direction)> {
    let mut conf = Configuration { ltape: Tape::new(), rtape: Tape::new(), dir: Direction::Right, sim_step: 0 };
    let mut active_tape_dir = Direction::Left;
    let mut tape = &mut conf.ltape;

    for token in s.split_whitespace() {
        if token.ends_with(":") {
            conf.sim_step = token[0..token.len() - 1].parse().unwrap();
            continue;
        }
        if token == "<" {
            assert_eq!(active_tape_dir, Direction::Left);
            active_tape_dir = Direction::Right;
            tape = &mut conf.rtape;

            conf.dir = Direction::Left;
            continue;
        }
        if token == ">" {
            assert_eq!(active_tape_dir, Direction::Left);
            active_tape_dir = Direction::Right;
            tape = &mut conf.rtape;

            conf.dir = Direction::Right;
            continue;
        }
        if let Some((block, exp)) = token.split_once("^") {
            if block == "x" {
                tape.push(Item::X(exp.parse()?));
            } else {
                tape.push(Item::E { block: block.chars().next().unwrap() as u8 - b'a', exp: exp.parse()? });
            }
            continue;
        }
        if let Some(encoded) = token.strip_prefix("L(") {
            tape.push(Item::L(encoded[..(encoded.len() - 1)].parse()?));
            continue;
        }
        tape.extend(token.chars().map(|symbol| match symbol {
            'D' => Item::D,
            'P' => Item::P,
            '0'..='9' => Item::C(symbol as u8 - b'0'),
            'x' => Item::X(1),
            '!' => Item::Unreachable,
            _ => unreachable!(),
        }));
    }
    Ok((conf, active_tape_dir))
}

fn parse(s: &str) -> Result<Tape> {
    let (conf, dir) = raw_parse(s)?;
    assert_eq!(dir, Direction::Left);

    Ok(conf.ltape)
}

impl FromStr for Configuration {
    type Err = color_eyre::Report;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let (mut conf, dir) = raw_parse(s)?;
        assert_eq!(dir, Direction::Right);
        conf.rtape.reverse();

        Ok(conf)
    }
}

#[derive(FromArgs, Debug)]
/// Let's simulate
struct Args {
    /// simulation step limit, 2^n
    #[argh(positional)]
    sim_step_limit: u32,
    /// how often to print configuration, 2^n
    #[argh(positional)]
    print_mod: u8,
    /// starting configuration
    #[argh(option, default = "Configuration::new()")]
    conf: Configuration,
    /// tui mode
    #[argh(switch, short = 't')]
    tui: bool,
}

#[derive(Clone, Copy, Debug)]
struct Config {
    sim_step_limit: usize,
    print_mod: u8,
}

// new run:               cargo run --release --bin no1 60 30
// explore configuration: cargo run --release --bin no1 8 0 --conf "2 x^7640 D x^10344 2 x^7640 D x^10344 1 x^7640 D x^10345 3 x^7639 D x^10347 3 < ! "
// explore in tui:        cargo run --release --bin no1 8 0 --conf "1 > P" --tui
fn main() -> Result<()> {
    color_eyre::install()?;

    // return transcode();

    let machine = Machine::from("1RB1RD_1LC0RC_1RA1LD_0RE0LB_---1RC");
    let args: Args = argh::from_env();
    let cfg = Config { sim_step_limit: 2usize.checked_pow(args.sim_step_limit).unwrap(), print_mod: args.print_mod };
    let mut conf = args.conf;
    // dbg!(cfg);
    println!("{}", conf);

    // let block_a = parse("011")?;
    // let block_b = parse("11 a^15282 01 a^20689")?;
    // let block_c = parse("a^144285 01 a^6153 01 a^3077 01 a^601 01 a^61653 01")?;
    // let mut block_d = parse("a^144285 10 a^6153 10 a^3077 10 a^601 10 a^61653 10")?;
    // block_d.reverse();
    // let block_e = parse("a^61651 001 a^1 01 a^144283 001 a^1 01 a^6151 001 a^1 01 a^3075 001 a^1 01 a^599 001 a^1 01")?;

    // let blocks =
    //     vec![block_a.as_slice(), block_b.as_slice(), block_c.as_slice(), block_d.as_slice(), block_e.as_slice()];
    let blocks = Vec::new();
    // dbg!(&blocks);

    if args.tui {
        tui(conf, &machine, &blocks, cfg)?;
    } else {
        let ret = conf.run(&machine, &blocks, cfg);
        println!("{conf}");
        dbg!(&ret);
    }

    Ok(())
}

fn tui(mut conf: Configuration, machine: &Machine, blocks: RefBlocks, mut cfg: Config) -> Result<()> {
    let stdin = io::stdin();
    let mut screen = io::stdout().into_raw_mode()?.into_alternate_screen()?;
    write!(screen, "{}", termion::cursor::Hide).unwrap();

    let mut speed = cfg.print_mod;
    cfg.print_mod = 63; // do not print inside conf::run

    let mut keys = stdin.keys();
    let mut state: Result<(), Err> = Ok(());
    let mut history: VecDeque<(u8, Result<(), Err>, Configuration)> = VecDeque::new();
    loop {
        write!(
            screen,
            "{}{}q: quit, j: next step, k: previous, h: slow down, l: speed up; current step speed: 2^{} == {}\r\n",
            termion::clear::All,
            termion::cursor::Goto(1, 1),
            speed,
            (1 << speed).bright_white()
        )?;
        write!(screen, "history size: {}, speed stack ('a' + speed):\r\n\r\n", history.len().bright_white(),)?;
        history.iter().take(100).rev().try_for_each(|(speed, _, _)| write!(screen, "{}", (speed + b'a') as char))?;
        write!(screen, "\r\n\r\nstate: {:?}\r\n\r\n{}", state.bright_white(), conf)?;
        screen.flush()?;

        match keys.next().unwrap().unwrap() {
            Key::Char('q') => break,
            Key::Char('j') if state.is_ok() || state.contains_err(&Err::StepLimit) => {
                let step = 1 << speed;
                cfg.sim_step_limit = conf.sim_step + step;
                state = conf.run(machine, blocks, cfg);
                if history.len() > 1_000_000 {
                    history.pop_back();
                }
                history.push_front((speed, state, conf.clone()));
            }
            Key::Char('k') => {
                if let Some((_, s, c)) = history.pop_front() {
                    state = s;
                    conf = c;
                }
            }
            Key::Char('h') => speed = speed.saturating_sub(1),
            Key::Char('l') => speed = (speed + 1).min(30),
            _ => {}
        }
    }
    write!(screen, "{}", termion::cursor::Show).unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_block() -> Result<()> {
        let mut b = parse("! x23 x^7640 DP x^10344 L(69)")?;
        b.reverse();
        assert_eq!(b, parse("L(69) x^10344 PD  x^7640 32x!")?);
        Ok(())
    }

    #[test]
    fn parse_conf() -> Result<()> {
        for inp in ["0:  2 x^3 P a^4 DD x^167 31 x^17 L(432)  >  3 x^70 P", "0: 0 < 1 !"] {
            let conf: Configuration = inp.parse()?;
            assert_eq!(
                inp.split_whitespace().collect::<String>(),
                String::from_utf8(strip_ansi_escapes::strip(conf.to_string())?)?.split_whitespace().collect::<String>()
            );
        }
        Ok(())
    }
}
