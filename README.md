# hardrt

Стартовый каркас репозитория для `Phase 0 / Week 1` нового hard real-time OS проекта.

Этот каркас соответствует стартовым шагам из исследовательского документа:
- workspace layout;
- `cargo xtask` + `justfile`;
- базовая QEMU/GDB scaffolding;
- `ai-docs/` и первый ADR;
- pinned toolchain и повторяемая структура репозитория.

## Что входит

- Rust workspace с базовыми crate'ами
- `tools/xtask` для инженерных команд
- `justfile` как удобный фронтенд
- `scripts/install-arch-deps.sh` для Arch Linux
- `AGENTS.md`
- `ai-docs/decision-records/ADR-0001-architecture-strategy.md`
- минимальный crate `bootinfo`
- минимальный `kernel` crate (`no_std`)

## Быстрый старт

```bash
./scripts/install-arch-deps.sh
rustup default stable
rustup component add rust-src rustfmt clippy
rustup target add aarch64-unknown-none x86_64-unknown-none riscv64gc-unknown-none-elf

cargo xtask doctor
just help
just phase0-check
```

## Статус

Это именно каркас для Week 1.
Здесь еще нет полноценного boot path и вывода stage marker из AArch64 kernel в QEMU — это следующий шаг (`Month 1 / Week 2`).

## Рекомендуемый старт git

```bash
git init
git add .
git commit -m "chore(repo): bootstrap hard RTOS workspace"
```
