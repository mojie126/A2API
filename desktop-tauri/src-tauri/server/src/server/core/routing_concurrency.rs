//! 组内并发：把路由配置的总并发按平均分配策略落到每个候选账号
//!
//! ── 默认总数 ────────────────────────────────────────────────
//! 组内并发默认关闭，未启用时不限制组内总并发，候选账号沿用自身的 `maxConcurrent`
//! 启用后配置未显式给出总数时，候选账号全部设置了并发上限（`maxConcurrent > 0`）
//! 取上限之和并锁定，否则取候选账号数量（每个账号一个并发槽位）
//!
//! ── 平均分配 ────────────────────────────────────────────────
//! 总数先在所有候选账号之间均分，除不尽的余数按候选顺序逐个补一
//! 分配结果必须不超过账号自己的并发上限，超出部分让给还有余量的账号
//! 会话粘滞只改变选路顺序，不改变账号分到的槽位数

use serde_json::Value;

use super::routing::max_concurrent_of;

/// 候选账号上的有效并发上限字段（覆盖账号自身的 `maxConcurrent`）
pub const CAP_KEY: &str = "routingConcurrencyCap";

/// 组内并发总数与是否被账号上限锁定（是 = 界面不可修改）
pub fn effective_total(accounts: &[Value], configured: Option<u64>) -> (u64, bool) {
    let caps: Vec<u64> = accounts.iter().map(max_concurrent_of).collect();
    let all_limited = !accounts.is_empty() && caps.iter().all(|cap| *cap > 0);
    if all_limited {
        (caps.iter().sum(), true)
    } else {
        (configured.unwrap_or(accounts.len() as u64), false)
    }
}

/// 按平均分配算出每个候选账号的并发槽位数（顺序与入参一致）
///
/// 账号上限为 0 表示不限，可以承接剩余的全部槽位
/// 账号上限小于均分份额时只拿走自己的上限，剩余槽位继续在其余账号间均分
pub fn allocations(accounts: &[Value], configured: Option<u64>) -> Vec<u64> {
    if accounts.is_empty() {
        return Vec::new();
    }
    let (total, _) = effective_total(accounts, configured);
    let mut capacity: Vec<(usize, u64)> = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let cap = max_concurrent_of(account);
            (index, if cap > 0 { cap } else { u64::MAX })
        })
        .collect();
    capacity.sort_by_key(|(index, cap)| (*cap, *index));
    let mut result = vec![0u64; accounts.len()];
    let mut remaining = total;
    let mut cursor = 0usize;
    while cursor < capacity.len() && remaining > 0 {
        let rest = (capacity.len() - cursor) as u64;
        let share = remaining / rest;
        let (index, cap) = capacity[cursor];
        if cap <= share {
            result[index] = cap;
            remaining -= cap;
            cursor += 1;
            continue;
        }
        let extra = remaining % rest;
        for (offset, (index, _)) in capacity[cursor..].iter().enumerate() {
            result[*index] = share + u64::from((offset as u64) < extra);
        }
        remaining = 0;
    }
    result
}

/// 为候选账号写入有效并发上限（`CAP_KEY`），未启用时不做任何限制
///
/// 启用后 `routing::concurrency_limit_of` 与账号自身的 `maxConcurrent` 共同生效
pub fn apply(accounts: &mut [Value], enabled: bool, configured: Option<u64>, strategy: Option<&str>) {
    if !enabled || strategy.is_some_and(|value| value != super::super::config::CONCURRENCY_STRATEGY_AVERAGE) {
        return;
    }
    let caps = allocations(accounts, configured);
    for (account, cap) in accounts.iter_mut().zip(caps) {
        if let Some(object) = account.as_object_mut() {
            object.insert(CAP_KEY.to_string(), Value::from(cap));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account(id: &str, max: Option<u64>) -> Value {
        match max {
            Some(max) => json!({ "id": id, "maxConcurrent": max }),
            None => json!({ "id": id }),
        }
    }

    #[test]
    fn the_default_total_is_the_account_count_when_any_account_is_unlimited() {
        let accounts = vec![account("a", Some(2)), account("b", None), account("c", Some(4))];
        assert_eq!(effective_total(&accounts, None), (3, false));
        let mut configured = accounts.clone();
        apply(&mut configured, true, None, None);
        assert_eq!(allocations(&accounts, None), vec![1, 1, 1]);
        assert_eq!(configured[0][CAP_KEY], 1);
    }

    /// 未启用时不写入并发上限，候选账号保持原有行为
    #[test]
    fn a_disabled_group_keeps_the_accounts_unlimited() {
        let mut accounts = vec![account("a", None), account("b", None)];
        apply(&mut accounts, false, Some(2), None);
        assert!(accounts.iter().all(|account| account.get(CAP_KEY).is_none()));
    }

    #[test]
    fn the_total_is_locked_to_the_sum_of_limits_when_every_account_is_limited() {
        let accounts = vec![account("a", Some(2)), account("b", Some(4))];
        assert_eq!(effective_total(&accounts, Some(9)), (6, true));
        assert_eq!(allocations(&accounts, Some(9)), vec![2, 4]);
    }

    #[test]
    fn the_average_split_puts_every_remainder_slot_on_the_earlier_accounts() {
        let accounts = vec![account("a", None), account("b", None), account("c", None), account("d", None)];
        assert_eq!(allocations(&accounts, Some(10)), vec![3, 3, 2, 2]);
    }

    #[test]
    fn an_allocation_never_exceeds_the_account_limit_and_the_surplus_moves_on() {
        let accounts = vec![account("a", Some(1)), account("b", None), account("c", None)];
        assert_eq!(allocations(&accounts, Some(7)), vec![1, 3, 3]);
    }

    #[test]
    fn a_total_smaller_than_the_account_count_leaves_the_tail_without_slots() {
        let accounts = vec![account("a", None), account("b", None), account("c", None)];
        assert_eq!(allocations(&accounts, Some(2)), vec![1, 1, 0]);
        let mut applied = accounts.clone();
        apply(&mut applied, true, Some(2), Some("average"));
        assert_eq!(applied[2][CAP_KEY], 0);
    }
}
