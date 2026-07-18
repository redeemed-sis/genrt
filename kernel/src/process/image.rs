use alloc::vec::Vec;
use core::mem;

use crate::{
    errno,
    memory::{
        self,
        user::{self, OwnedUserStack, USER_STACK_TOP},
    },
};

use super::{
    USER_STACK_SIZE,
    error::{exec_arg_copy_errno, exec_user_copy_errno},
};

const EXEC_STACK_WORD_BYTES: usize = mem::size_of::<u64>();
const EXEC_STACK_ARGC_WORDS: usize = 1;
const EXEC_STACK_ARGV_NULL_WORDS: usize = 1;
const EXEC_STACK_ENVP_NULL_WORDS: usize = 1;

pub(crate) struct ExecArgs {
    pub(super) argv: Vec<Vec<u8>>,
    pub(super) envp: Vec<Vec<u8>>,
    string_bytes: usize,
}

#[derive(Copy, Clone)]
pub(super) enum ExecStringVector {
    Argv,
    Envp,
}

impl ExecArgs {
    pub(super) fn empty() -> Self {
        Self {
            argv: Vec::new(),
            envp: Vec::new(),
            string_bytes: 0,
        }
    }

    pub(super) fn push(
        &mut self,
        vector: ExecStringVector,
        value: Vec<u8>,
    ) -> Result<(), errno::Errno> {
        let bytes_with_nul = value.len().checked_add(1).ok_or(errno::E2BIG)?;
        let next_argv =
            self.argv.len() + exec_vector_slot_increment(vector, ExecStringVector::Argv);
        let next_envp =
            self.envp.len() + exec_vector_slot_increment(vector, ExecStringVector::Envp);
        let budget = exec_arg_string_budget(next_argv, next_envp).ok_or(errno::E2BIG)?;
        let next_bytes = self
            .string_bytes
            .checked_add(bytes_with_nul)
            .ok_or(errno::E2BIG)?;
        if next_bytes > budget {
            return Err(errno::E2BIG);
        }
        match vector {
            ExecStringVector::Argv => {
                self.argv.try_reserve_exact(1).map_err(|_| errno::ENOMEM)?;
                self.argv.push(value);
            }
            ExecStringVector::Envp => {
                self.envp.try_reserve_exact(1).map_err(|_| errno::ENOMEM)?;
                self.envp.push(value);
            }
        }
        self.string_bytes = next_bytes;
        Ok(())
    }

    fn remaining_for_next(&self, vector: ExecStringVector) -> Result<usize, errno::Errno> {
        let next_argv =
            self.argv.len() + exec_vector_slot_increment(vector, ExecStringVector::Argv);
        let next_envp =
            self.envp.len() + exec_vector_slot_increment(vector, ExecStringVector::Envp);
        let budget = exec_arg_string_budget(next_argv, next_envp).ok_or(errno::E2BIG)?;
        budget.checked_sub(self.string_bytes).ok_or(errno::E2BIG)
    }
}

pub(super) fn copy_exec_args_from_user(
    path: &[u8],
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<ExecArgs, errno::Errno> {
    let mut args = ExecArgs::empty();
    if argv_ptr == 0 {
        args.push(ExecStringVector::Argv, path.to_vec())?;
    } else {
        copy_exec_string_vector_from_user(argv_ptr, ExecStringVector::Argv, &mut args)?;
    }
    if envp_ptr != 0 {
        copy_exec_string_vector_from_user(envp_ptr, ExecStringVector::Envp, &mut args)?;
    }
    Ok(args)
}

pub(super) fn build_initial_user_stack(stack: &OwnedUserStack, args: &ExecArgs) -> Option<usize> {
    let stack_base = stack.base();
    let argc = args.argv.len();
    let envc = args.envp.len();
    let mut sp = USER_STACK_TOP;
    let mut arg_ptrs = Vec::new();
    let mut env_ptrs = Vec::new();
    arg_ptrs.try_reserve_exact(argc).ok()?;
    env_ptrs.try_reserve_exact(envc).ok()?;
    for env in args.envp.iter().rev() {
        env_ptrs.push(push_stack_cstr(stack, stack_base, &mut sp, env)? as u64);
    }
    env_ptrs.reverse();
    for arg in args.argv.iter().rev() {
        arg_ptrs.push(push_stack_cstr(stack, stack_base, &mut sp, arg)? as u64);
    }
    arg_ptrs.reverse();
    sp &= !0xf;
    let table_size = exec_stack_table_words(argc, envc)?.checked_mul(EXEC_STACK_WORD_BYTES)?;
    sp = sp.checked_sub(table_size)? & !0xf;
    if sp < stack_base {
        return None;
    }
    write_stack_u64(stack, stack_base, sp, argc as u64)?;
    let mut word = 1usize;
    for ptr in arg_ptrs {
        write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, ptr)?;
        word += 1;
    }
    write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, 0)?;
    word += 1;
    for ptr in env_ptrs {
        write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, ptr)?;
        word += 1;
    }
    write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, 0)?;
    Some(sp)
}

fn push_stack_cstr(
    stack: &OwnedUserStack,
    stack_base: usize,
    sp: &mut usize,
    bytes: &[u8],
) -> Option<usize> {
    *sp = (*sp).checked_sub(bytes.len().checked_add(1)?)?;
    if *sp < stack_base {
        return None;
    }
    write_stack_bytes(stack, stack_base, *sp, bytes)?;
    write_stack_byte(stack, stack_base, *sp + bytes.len(), 0)?;
    Some(*sp)
}

fn write_stack_bytes(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    bytes: &[u8],
) -> Option<()> {
    if bytes.is_empty() {
        return Some(());
    }
    let offset = user_va.checked_sub(stack_base)?;
    if offset.checked_add(bytes.len())? > USER_STACK_SIZE {
        return None;
    }
    memory::copy_bytes_to_phys(stack.frames().start + offset, bytes);
    Some(())
}

fn write_stack_byte(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    byte: u8,
) -> Option<()> {
    let offset = user_va.checked_sub(stack_base)?;
    if offset >= USER_STACK_SIZE {
        return None;
    }
    memory::copy_bytes_to_phys(stack.frames().start + offset, &[byte]);
    Some(())
}

fn write_stack_u64(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    value: u64,
) -> Option<()> {
    write_stack_bytes(stack, stack_base, user_va, &value.to_le_bytes())
}

fn exec_stack_table_words(argc: usize, envc: usize) -> Option<usize> {
    EXEC_STACK_ARGC_WORDS
        .checked_add(argc)?
        .checked_add(EXEC_STACK_ARGV_NULL_WORDS)?
        .checked_add(envc)?
        .checked_add(EXEC_STACK_ENVP_NULL_WORDS)
}

fn exec_arg_string_budget(argc: usize, envc: usize) -> Option<usize> {
    let table_bytes = exec_stack_table_words(argc, envc)?.checked_mul(EXEC_STACK_WORD_BYTES)?;
    USER_STACK_SIZE.checked_sub(table_bytes)
}

fn exec_vector_slot_increment(vector: ExecStringVector, target: ExecStringVector) -> usize {
    if matches!(
        (vector, target),
        (ExecStringVector::Argv, ExecStringVector::Argv)
            | (ExecStringVector::Envp, ExecStringVector::Envp)
    ) {
        1
    } else {
        0
    }
}

fn stack_word_addr(sp: usize, word: usize) -> Option<usize> {
    sp.checked_add(word.checked_mul(EXEC_STACK_WORD_BYTES)?)
}

fn read_user_usize(ptr: usize) -> Result<usize, errno::Errno> {
    let mut bytes = [0u8; mem::size_of::<usize>()];
    user::copy_from_user(&mut bytes, ptr).map_err(exec_user_copy_errno)?;
    Ok(usize::from_le_bytes(bytes))
}

fn copy_exec_string_vector_from_user(
    vector_ptr: usize,
    vector: ExecStringVector,
    args: &mut ExecArgs,
) -> Result<(), errno::Errno> {
    let mut index = 0usize;
    loop {
        let ptr = read_user_usize(
            vector_ptr
                .checked_add(
                    index
                        .checked_mul(mem::size_of::<usize>())
                        .ok_or(errno::EFAULT)?,
                )
                .ok_or(errno::EFAULT)?,
        )?;
        if ptr == 0 {
            return Ok(());
        }
        let remaining = args.remaining_for_next(vector)?;
        let max_string_len = remaining.checked_sub(1).ok_or(errno::E2BIG)?;
        let value = user::copy_cstr_from_user(ptr, max_string_len).map_err(exec_arg_copy_errno)?;
        args.push(vector, value)?;
        index = index.checked_add(1).ok_or(errno::E2BIG)?;
    }
}
