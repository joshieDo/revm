//! Integration tests for the `revm` crate.

use crate::TestdataConfig;
use revm::{
    bytecode::opcode,
    context::{CfgEnv, ContextTr, TxEnv},
    database::{BenchmarkDB, BENCH_CALLER, BENCH_TARGET},
    primitives::{address, b256, hardfork::SpecId, Bytes, TxKind, KECCAK_EMPTY, U256},
    state::{AccountStatus, Bytecode},
    Context, ExecuteEvm, MainBuilder, MainContext,
};
use std::path::PathBuf;

// Re-export the constant for testdata directory path
const TESTS_TESTDATA: &str = "tests/revm_testdata";

fn revm_testdata_config() -> TestdataConfig {
    TestdataConfig {
        testdata_dir: PathBuf::from(TESTS_TESTDATA),
    }
}

fn compare_or_save_revm_testdata<T>(filename: &str, output: &T)
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a> + PartialEq + std::fmt::Debug,
{
    crate::compare_or_save_testdata_with_config(filename, output, revm_testdata_config());
}

const SELFDESTRUCT_BYTECODE: &[u8] = &[
    opcode::PUSH2,
    0xFF,
    0xFF,
    opcode::SELFDESTRUCT,
    opcode::STOP,
];

#[test]
fn test_selfdestruct_multi_tx() {
    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::BERLIN))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new_legacy(
            SELFDESTRUCT_BYTECODE.into(),
        )))
        .build_mainnet();

    // trigger selfdestruct
    let result1 = evm
        .transact_one(TxEnv::builder_for_bench().build_fill())
        .unwrap();

    let destroyed_acc = evm.ctx.journal_mut().state.get_mut(&BENCH_TARGET).unwrap();

    // balance got transferred to 0x0000..00FFFF
    assert_eq!(destroyed_acc.info.balance, U256::ZERO);
    assert_eq!(destroyed_acc.info.nonce, 1);
    assert_eq!(
        destroyed_acc.info.code_hash,
        b256!("0x9125466aa9ef15459d85e7318f6d3bdc5f6978c0565bee37a8e768d7c202a67a")
    );

    // call on destroyed account. This accounts gets loaded and should contain empty code_hash afterwards.
    let result2 = evm
        .transact_one(TxEnv::builder_for_bench().nonce(1).build_fill())
        .unwrap();

    let destroyed_acc = evm.ctx.journal_mut().state.get_mut(&BENCH_TARGET).unwrap();

    assert_eq!(destroyed_acc.info.code_hash, KECCAK_EMPTY);
    assert_eq!(destroyed_acc.info.nonce, 0);
    assert_eq!(destroyed_acc.info.code, Some(Bytecode::default()));

    let output = evm.finalize();

    compare_or_save_revm_testdata(
        "test_selfdestruct_multi_tx.json",
        &(result1, result2, output),
    );
}

/// Tests multiple transactions with contract creation.
/// Verifies that created contracts persist correctly across transactions
/// and that their state is properly maintained.
#[test]
fn test_multi_tx_create() {
    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| {
            cfg.set_spec_and_mainnet_gas_params(SpecId::BERLIN);
            cfg.disable_nonce_check = true;
        })
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new()))
        .build_mainnet();

    let result1 = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .kind(TxKind::Create)
                .data(deployment_contract(SELFDESTRUCT_BYTECODE))
                .build_fill(),
        )
        .unwrap();

    let created_address = result1.created_address().unwrap();

    let created_acc = evm
        .ctx
        .journal_mut()
        .state
        .get_mut(&created_address)
        .unwrap();

    assert_eq!(
        created_acc.status,
        AccountStatus::Created
            | AccountStatus::CreatedLocal
            | AccountStatus::Touched
            | AccountStatus::LoadedAsNotExisting
    );

    let result2 = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .nonce(1)
                .kind(TxKind::Call(created_address))
                .build_fill(),
        )
        .unwrap();

    let created_acc = evm
        .ctx
        .journal_mut()
        .state
        .get_mut(&created_address)
        .unwrap();

    // reset nonce to trigger create on same address.
    assert_eq!(
        created_acc.status,
        AccountStatus::Created
            | AccountStatus::SelfDestructed
            | AccountStatus::SelfDestructedLocal
            | AccountStatus::Touched
            | AccountStatus::LoadedAsNotExisting
    );

    // reset caller nonce
    evm.ctx
        .journal_mut()
        .state
        .get_mut(&BENCH_CALLER)
        .unwrap()
        .info
        .nonce = 0;

    // re create the contract.
    let result3 = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .nonce(0)
                .kind(TxKind::Create)
                .data(deployment_contract(SELFDESTRUCT_BYTECODE))
                .build_fill(),
        )
        .unwrap();

    let created_address_new = result3.created_address().unwrap();
    assert_eq!(created_address, created_address_new);

    let created_acc = evm
        .ctx
        .journal_mut()
        .state
        .get_mut(&created_address)
        .unwrap();

    assert_eq!(
        created_acc.status,
        AccountStatus::Created
            | AccountStatus::CreatedLocal
            | AccountStatus::Touched
            | AccountStatus::SelfDestructed
            | AccountStatus::LoadedAsNotExisting
    );
    let output = evm.finalize();

    compare_or_save_revm_testdata(
        "test_multi_tx_create.json",
        &(result1, result2, result3, output),
    );
}

/// Creates deployment bytecode for a contract.
/// Prepends the initialization code that will deploy the provided runtime bytecode.
fn deployment_contract(bytes: &[u8]) -> Bytes {
    assert!(bytes.len() < 256);
    let len = bytes.len();
    let ret = &[
        opcode::PUSH1,
        len as u8,
        opcode::PUSH1,
        12,
        opcode::PUSH1,
        0,
        // Copy code to memory.
        opcode::CODECOPY,
        opcode::PUSH1,
        len as u8,
        opcode::PUSH1,
        0,
        // Return copied code.
        opcode::RETURN,
    ];

    [ret, bytes].concat().into()
}

#[test]
fn test_frame_stack_index() {
    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::BERLIN))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new_legacy(
            SELFDESTRUCT_BYTECODE.into(),
        )))
        .build_mainnet();

    // transfer to other account
    let result1 = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .to(address!("0xc000000000000000000000000000000000000000"))
                .build_fill(),
        )
        .unwrap();

    assert_eq!(evm.frame_stack.index(), None);
    compare_or_save_revm_testdata("test_frame_stack_index.json", &result1);
}

#[test]
#[cfg(feature = "optional_balance_check")]
fn test_disable_balance_check() {
    use revm::database::BENCH_CALLER_BALANCE;
    const RETURN_CALLER_BALANCE_BYTECODE: &[u8] = &[
        opcode::CALLER,
        opcode::BALANCE,
        opcode::PUSH1,
        0x00,
        opcode::MSTORE,
        opcode::PUSH1,
        0x20,
        opcode::PUSH1,
        0x00,
        opcode::RETURN,
    ];

    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| cfg.disable_balance_check = true)
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new_legacy(
            RETURN_CALLER_BALANCE_BYTECODE.into(),
        )))
        .build_mainnet();

    // Construct tx so that effective cost is more than caller balance.
    let gas_price = 1;
    let gas_limit = 100_000;
    // Make sure value doesn't consume all balance since we want to validate that all effective
    // cost is deducted.
    let tx_value = BENCH_CALLER_BALANCE - U256::from(1);

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .gas_price(gas_price)
                .gas_limit(gas_limit)
                .value(tx_value)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success());

    let returned_balance = U256::from_be_slice(result.output().unwrap().as_ref());
    let expected_balance = U256::ZERO;
    assert_eq!(returned_balance, expected_balance);
}

// ============================================================================
// EIP-7708: ETH transfers emit a log
// ============================================================================

use revm::primitives::eip7708::{BURN_LOG_TOPIC, ETH_TRANSFER_LOG_ADDRESS, ETH_TRANSFER_LOG_TOPIC};
use revm::primitives::B256;

/// Test EIP-7708 transfer log emission for transaction value transfer
#[test]
fn test_eip7708_transfer_log_tx_value() {
    let recipient = address!("0xc000000000000000000000000000000000000001");

    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new()))
        .build_mainnet();

    let tx_value = U256::from(1_000_000_000_000_000u128); // 0.001 ETH (within balance)

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .to(recipient)
                .value(tx_value)
                .gas_limit(100_000)
                .gas_price(0) // Zero gas price to avoid balance issues
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success());

    // Verify that a transfer log was emitted
    let logs = result.logs();
    assert_eq!(logs.len(), 1, "Expected 1 transfer log");

    let log = &logs[0];
    assert_eq!(log.address, ETH_TRANSFER_LOG_ADDRESS);
    assert_eq!(log.data.topics().len(), 3);
    assert_eq!(log.data.topics()[0], ETH_TRANSFER_LOG_TOPIC);
    assert_eq!(
        log.data.topics()[1],
        B256::left_padding_from(BENCH_CALLER.as_slice())
    );
    assert_eq!(
        log.data.topics()[2],
        B256::left_padding_from(recipient.as_slice())
    );
    assert_eq!(log.data.data.as_ref(), &tx_value.to_be_bytes::<32>());
}

/// Test that no transfer log is emitted for zero value transfer
#[test]
fn test_eip7708_no_log_for_zero_value() {
    let recipient = address!("0xc000000000000000000000000000000000000001");

    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new()))
        .build_mainnet();

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .to(recipient)
                .value(U256::ZERO)
                .gas_limit(100_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success());

    // No logs should be emitted for zero value transfer
    let logs = result.logs();
    assert_eq!(logs.len(), 0, "Expected no logs for zero value transfer");
}

/// Test that no transfer log is emitted before AMSTERDAM
#[test]
fn test_eip7708_no_log_before_amsterdam() {
    let recipient = address!("0xc000000000000000000000000000000000000001");

    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::OSAKA)) // Before AMSTERDAM
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new()))
        .build_mainnet();

    let tx_value = U256::from(1_000_000_000_000_000u128); // 0.001 ETH (within balance)

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .to(recipient)
                .value(tx_value)
                .gas_limit(100_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success());

    // No logs should be emitted before AMSTERDAM
    let logs = result.logs();
    assert_eq!(logs.len(), 0, "Expected no logs before AMSTERDAM");
}

/// Bytecode that selfdestructs to the caller (different address)
const SELFDESTRUCT_TO_CALLER_BYTECODE: &[u8] = &[
    opcode::CALLER, // Push caller address
    opcode::SELFDESTRUCT,
    opcode::STOP,
];

/// Test EIP-7708 transfer log emission for selfdestruct to different address
#[test]
fn test_eip7708_selfdestruct_to_different_address() {
    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new_legacy(
            SELFDESTRUCT_TO_CALLER_BYTECODE.into(),
        )))
        .build_mainnet();

    // Execute selfdestruct - the contract at BENCH_TARGET will selfdestruct to BENCH_CALLER
    // After Cancun (and Amsterdam), SELFDESTRUCT only destroys if created in same tx,
    // but it still transfers balance, so we should see a transfer log.
    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .gas_limit(100_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success());

    // There should be two logs:
    // 1. Transfer log for initial tx value transfer from BENCH_CALLER to BENCH_TARGET
    // 2. Transfer log for selfdestruct balance transfer from BENCH_TARGET to BENCH_CALLER
    let logs = result.logs();

    // Find the selfdestruct transfer log (from BENCH_TARGET to BENCH_CALLER)
    let selfdestruct_log = logs.iter().find(|log| {
        log.data.topics().len() == 3
            && log.data.topics()[0] == ETH_TRANSFER_LOG_TOPIC
            && log.data.topics()[1] == B256::left_padding_from(BENCH_TARGET.as_slice())
    });

    assert!(
        selfdestruct_log.is_some(),
        "Expected selfdestruct transfer log"
    );
    let log = selfdestruct_log.unwrap();
    assert_eq!(log.address, ETH_TRANSFER_LOG_ADDRESS);
    assert_eq!(
        log.data.topics()[2],
        B256::left_padding_from(BENCH_CALLER.as_slice())
    );
}

/// Reproduces a bug where `RevertToSlot::Destroyed` is produced for a
/// pre-existing DB storage slot when a SELFDESTRUCT → CREATE2 cycle occurs
/// as the first block of a batch.
///
/// End-to-end test using real EVM execution with two transactions:
/// 1. Tx1: Calls the child contract which SELFDESTRUCTs
/// 2. Tx2: Factory CREATE2s the child back with SSTORE(slot0, 1)
/// 3. The bundle revert should record Some(1) for slot(0) but instead
///    records Destroyed, which maps to 0 on unwind.
///
/// Inspired by Ethereum mainnet block 10,094,566.
#[test]
fn selfdestruct_create2_revert_loses_db_storage_value() {
    use revm::{
        bytecode::opcode,
        context::{CfgEnv, Context, TxEnv},
        database::{
            states::bundle_state::BundleRetention, InMemoryDB, RevertToSlot, State,
        },
        primitives::{address, hardfork::SpecId, Bytes, StorageKey, StorageValue, TxKind},
        state::{AccountInfo, Bytecode},
        ExecuteCommitEvm, MainBuilder, MainContext,
    };

    // ── Child contract runtime bytecode ──
    // SELFDESTRUCTs to caller when called.
    let child_runtime: &[u8] = &[
        opcode::CALLER,
        opcode::SELFDESTRUCT,
    ];

    // ── Child init code ──
    // SSTORE(0, 1) during init, then deploys runtime bytecode.
    let runtime_len = child_runtime.len() as u8;
    let init_code_bytes: Vec<u8> = {
        let mut code = Vec::new();
        // SSTORE(0, 1)
        code.extend_from_slice(&[opcode::PUSH1, 0x01, opcode::PUSH1, 0x00, opcode::SSTORE]);
        // CODECOPY(destOffset=0, offset=<after_return>, size=runtime_len)
        let header_len = 5 + 12; // 5 for SSTORE above, 12 for CODECOPY+RETURN below
        code.extend_from_slice(&[
            opcode::PUSH1, runtime_len,
            opcode::PUSH1, header_len as u8,
            opcode::PUSH1, 0x00,
            opcode::CODECOPY,
            opcode::PUSH1, runtime_len,
            opcode::PUSH1, 0x00,
            opcode::RETURN,
        ]);
        code.extend_from_slice(child_runtime);
        code
    };
    let child_init_code: Bytes = init_code_bytes.into();
    let child_init_code_len = child_init_code.len();

    let factory_addr = address!("0x1000000000000000000000000000000000000000");
    let caller = address!("0xf000000000000000000000000000000000000000");
    let salt = [0u8; 32];
    let child_addr = factory_addr.create2_from_code(salt, &child_init_code);

    // ── Factory runtime: stores init code in memory then CREATE2 ──
    let mut factory_code = Vec::new();
    for (i, &b) in child_init_code.iter().enumerate() {
        factory_code.extend_from_slice(&[opcode::PUSH1, b, opcode::PUSH1, i as u8, opcode::MSTORE8]);
    }
    // CREATE2(value=0, offset=0, size=len, salt=0)
    factory_code.push(opcode::PUSH32);
    factory_code.extend_from_slice(&salt);
    factory_code.extend_from_slice(&[opcode::PUSH1, child_init_code_len as u8]);
    factory_code.extend_from_slice(&[opcode::PUSH1, 0x00]); // offset
    factory_code.extend_from_slice(&[opcode::PUSH1, 0x00]); // value
    factory_code.extend_from_slice(&[opcode::CREATE2, opcode::POP, opcode::STOP]);

    // ── Set up DB with pre-existing child ──
    let mut db = InMemoryDB::default();
    db.insert_account_info(caller, AccountInfo {
        balance: U256::from(1_000_000_000_000_000_000u128),
        ..Default::default()
    });
    db.insert_account_info(factory_addr, AccountInfo {
        code: Some(Bytecode::new_legacy(factory_code.into())),
        ..Default::default()
    });
    // Child exists in DB with slot(0) = 1
    db.insert_account_info(child_addr, AccountInfo {
        nonce: 1,
        code: Some(Bytecode::new_legacy(child_runtime.into())),
        ..Default::default()
    });
    db.insert_account_storage(child_addr, StorageKey::default(), StorageValue::from(1))
        .unwrap();

    // ── Wrap in State for bundle tracking ──
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    let mut evm = Context::mainnet()
        .modify_cfg_chained(|cfg| {
            cfg.set_spec_and_mainnet_gas_params(SpecId::BERLIN);
            cfg.disable_nonce_check = true;
        })
        .with_db(&mut state)
        .build_mainnet();

    // Tx1: Call child → triggers SELFDESTRUCT
    let result1 = evm.transact_commit(
        TxEnv::builder()
            .caller(caller)
            .kind(TxKind::Call(child_addr))
            .gas_limit(100_000)
            .gas_price(0)
            .build()
            .unwrap(),
    ).unwrap();
    assert!(result1.is_success(), "Tx1 (selfdestruct) failed: {result1:?}");

    // Tx2: Call factory → CREATE2 recreates child with SSTORE(0, 1)
    let result2 = evm.transact_commit(
        TxEnv::builder()
            .caller(caller)
            .kind(TxKind::Call(factory_addr))
            .gas_limit(1_000_000)
            .gas_price(0)
            .nonce(1)
            .build()
            .unwrap(),
    ).unwrap();
    assert!(result2.is_success(), "Tx2 (create2) failed: {result2:?}");

    // ── Merge transitions into bundle ──
    drop(evm);
    state.merge_transitions(BundleRetention::Reverts);
    let bundle = state.take_bundle();

    // ── Verify the bundle state: child should exist with slot(0) = 1 ──
    let child_account = bundle.account(&child_addr).expect("child should be in bundle state");
    let slot_value = child_account
        .storage
        .get(&StorageKey::default())
        .expect("slot(0) should be in child's bundle storage");
    assert_eq!(
        slot_value.present_value,
        StorageValue::from(1),
        "slot(0) should be 1 after CREATE2 + SSTORE"
    );

    // ── Verify the revert ──
    let child_revert = bundle
        .reverts
        .iter()
        .flatten()
        .find(|(addr, _)| *addr == child_addr);

    assert!(
        child_revert.is_some(),
        "Expected revert entry for child contract at {child_addr}"
    );
    let (_, revert) = child_revert.unwrap();

    let slot_revert = revert.storage.get(&StorageKey::default());
    if let Some(slot_revert) = slot_revert {
        // BUG: We get Destroyed (→ to_previous_value() = 0) instead of Some(1).
        // On unwind, reth would write 0 to slot(0) instead of restoring 1.
        //
        // Expected:
        // assert_eq!(*slot_revert, RevertToSlot::Some(StorageValue::from(1)));
        //
        // Actual:
        assert_eq!(
            *slot_revert,
            RevertToSlot::Destroyed,
            "BUG: slot(0) revert should be Some(1) but got Destroyed"
        );
    } else {
        panic!("slot(0) missing from revert entirely");
    }
}

/// Init code that selfdestructs to itself during construction
/// This triggers the selfdestruct-to-self scenario where a newly created
/// contract (is_created_locally = true) selfdestructs to itself.
const SELFDESTRUCT_TO_SELF_INIT_CODE: &[u8] = &[
    opcode::ADDRESS, // Push contract's own address
    opcode::SELFDESTRUCT,
];

/// Test EIP-7708 selfdestruct-to-self log emission
/// This test creates a contract with value that selfdestructs to itself during construction,
/// which should emit a SelfBalanceLog since the contract is created in the same tx.
#[test]
fn test_eip7708_selfdestruct_to_self() {
    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(Bytecode::new()))
        .build_mainnet();

    let create_value = U256::from(1_000_000u128); // 1M wei

    // Create a contract with value that selfdestructs to itself during init
    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .kind(TxKind::Create)
                .data(SELFDESTRUCT_TO_SELF_INIT_CODE.into())
                .value(create_value)
                .gas_limit(100_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success(), "Transaction should succeed");

    // Find the burn log
    let logs = result.logs();
    let burn_log = logs
        .iter()
        .find(|log| log.data.topics().len() == 2 && log.data.topics()[0] == BURN_LOG_TOPIC);

    assert!(
        burn_log.is_some(),
        "Expected burn log, got logs: {:?}",
        logs
    );
    let log = burn_log.unwrap();
    assert_eq!(log.address, ETH_TRANSFER_LOG_ADDRESS);
    // The log data should contain the create value
    assert_eq!(log.data.data.as_ref(), &create_value.to_be_bytes::<32>());
}

/// Bytecode that performs a CALL with value to a specific address
#[allow(clippy::vec_init_then_push)]
fn call_with_value_bytecode(target: [u8; 20], value: U256) -> Bytecode {
    // CALL(gas, addr, value, argsOffset, argsSize, retOffset, retSize)
    let mut bytecode = Vec::new();

    // Push return size (0)
    bytecode.push(opcode::PUSH1);
    bytecode.push(0);

    // Push return offset (0)
    bytecode.push(opcode::PUSH1);
    bytecode.push(0);

    // Push args size (0)
    bytecode.push(opcode::PUSH1);
    bytecode.push(0);

    // Push args offset (0)
    bytecode.push(opcode::PUSH1);
    bytecode.push(0);

    // Push value (32 bytes)
    let value_bytes = value.to_be_bytes::<32>();
    bytecode.push(opcode::PUSH32);
    bytecode.extend_from_slice(&value_bytes);

    // Push target address (20 bytes)
    bytecode.push(opcode::PUSH20);
    bytecode.extend_from_slice(&target);

    // Push gas (use all remaining gas)
    bytecode.push(opcode::GAS);

    // Execute CALL
    bytecode.push(opcode::CALL);

    // Clean up stack
    bytecode.push(opcode::POP);

    // Stop
    bytecode.push(opcode::STOP);

    Bytecode::new_legacy(bytecode.into())
}

/// Test EIP-7708 transfer log emission for CALL with value
#[test]
fn test_eip7708_call_with_value() {
    let call_target = address!("0xd000000000000000000000000000000000000001");
    let call_value = U256::from(1_000_000u128); // Small value (1M wei)

    let bytecode = call_with_value_bytecode(call_target.into_array(), call_value);

    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(bytecode))
        .build_mainnet();

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .gas_limit(200_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success(), "Transaction should succeed");

    // There should be transfer logs:
    // Transfer from BENCH_TARGET to call_target (CALL value transfer)
    let logs = result.logs();

    // Find the CALL transfer log (from BENCH_TARGET to call_target)
    let call_log = logs.iter().find(|log| {
        log.data.topics().len() == 3
            && log.data.topics()[0] == ETH_TRANSFER_LOG_TOPIC
            && log.data.topics()[1] == B256::left_padding_from(BENCH_TARGET.as_slice())
            && log.data.topics()[2] == B256::left_padding_from(call_target.as_slice())
    });

    assert!(
        call_log.is_some(),
        "Expected CALL transfer log, got logs: {:?}",
        logs
    );
    let log = call_log.unwrap();
    assert_eq!(log.address, ETH_TRANSFER_LOG_ADDRESS);
    assert_eq!(log.data.data.as_ref(), &call_value.to_be_bytes::<32>());
}

/// Bytecode that creates a contract with initial value
#[allow(clippy::vec_init_then_push)]
fn create_with_value_bytecode(init_code: &[u8], value: U256) -> Bytecode {
    // CREATE(value, offset, length)
    let mut bytecode = Vec::new();

    // First, store init_code in memory
    // PUSH init_code bytes
    for (i, byte) in init_code.iter().enumerate() {
        bytecode.push(opcode::PUSH1);
        bytecode.push(*byte);
        bytecode.push(opcode::PUSH1);
        bytecode.push(i as u8);
        bytecode.push(opcode::MSTORE8);
    }

    // Push length
    bytecode.push(opcode::PUSH1);
    bytecode.push(init_code.len() as u8);

    // Push offset (0)
    bytecode.push(opcode::PUSH1);
    bytecode.push(0);

    // Push value (32 bytes)
    let value_bytes = value.to_be_bytes::<32>();
    bytecode.push(opcode::PUSH32);
    bytecode.extend_from_slice(&value_bytes);

    // Execute CREATE
    bytecode.push(opcode::CREATE);

    // Clean up stack
    bytecode.push(opcode::POP);

    // Stop
    bytecode.push(opcode::STOP);

    Bytecode::new_legacy(bytecode.into())
}

/// Simple init code that just returns nothing (creates empty contract)
const SIMPLE_INIT_CODE: &[u8] = &[
    opcode::PUSH1,
    0, // length
    opcode::PUSH1,
    0, // offset
    opcode::RETURN,
];

/// Test EIP-7708 transfer log emission for CREATE with value
#[test]
fn test_eip7708_create_with_value() {
    let create_value = U256::from(1_000_000u128); // Small value (1M wei)

    let bytecode = create_with_value_bytecode(SIMPLE_INIT_CODE, create_value);

    let mut evm = Context::mainnet()
        .with_cfg(CfgEnv::new_with_spec(SpecId::AMSTERDAM))
        .with_db(BenchmarkDB::new_bytecode(bytecode))
        .build_mainnet();

    let result = evm
        .transact_one(
            TxEnv::builder_for_bench()
                .gas_limit(200_000)
                .gas_price(0)
                .build_fill(),
        )
        .unwrap();

    assert!(result.is_success(), "Transaction should succeed");

    // There should be transfer logs:
    // Transfer from BENCH_TARGET to created address (CREATE value transfer)
    let logs = result.logs();

    // Find the CREATE transfer log (from BENCH_TARGET to some address)
    let create_log = logs.iter().find(|log| {
        log.data.topics().len() == 3
            && log.data.topics()[0] == ETH_TRANSFER_LOG_TOPIC
            && log.data.topics()[1] == B256::left_padding_from(BENCH_TARGET.as_slice())
            && log.data.topics()[2] != B256::left_padding_from(BENCH_CALLER.as_slice())
    });

    assert!(
        create_log.is_some(),
        "Expected CREATE transfer log, got logs: {:?}",
        logs
    );
    let log = create_log.unwrap();
    assert_eq!(log.address, ETH_TRANSFER_LOG_ADDRESS);
    assert_eq!(log.data.data.as_ref(), &create_value.to_be_bytes::<32>());
}
