# genrt

Стартовый каркас репозитория для `Phase 0 / Week 1` и минимального bring-up для `Week 2` проекта `genrt`.

Этот каркас соответствует стартовым шагам из исследовательского документа:
- workspace layout;
- `cargo xtask` + `justfile`;
- базовая QEMU/GDB scaffolding;
- `ai-docs/` и первый ADR;
- pinned toolchain и повторяемая структура репозитория;
- минимальный AArch64 boot path для QEMU `virt`.

## Что входит

- Rust workspace с базовыми crate'ами
- `tools/xtask` для инженерных команд
- `justfile` как удобный фронтенд
- `scripts/install-arch-deps.sh` для Arch Linux
- `AGENTS.md`
- `ai-docs/decision-records/ADR-0001-architecture-strategy.md`
- `bootinfo` crate
- `kernel` crate (`no_std`)
- `arch/aarch64` с `_start`, linker script и ранним входом в Rust

## Быстрый старт

```bash
./scripts/install-arch-deps.sh
rustup default stable
rustup component add rust-src rustfmt clippy llvm-tools
rustup target add aarch64-unknown-none x86_64-unknown-none riscv64gc-unknown-none-elf

cargo xtask doctor
just help
just phase0-check
just run-aarch64
```

## Статус

После применения патча у проекта появляется минимальный boot path для `Month 1 / Week 2`:
- загрузка в QEMU `virt`;
- ранний `_start` для AArch64;
- передача DTB pointer в Rust;
- вывод stage marker через PL011;
- цикл `run/debug/gdb`.

## Рекомендуемый старт git

```bash
git init
git add .
git commit -m "chore(genrt): bootstrap hard RTOS workspace"
```
