#![cfg(feature = "test-bpf")]

mod helpers;

use {
    bincode::deserialize,
    borsh::BorshSerialize,
    helpers::*,
    solana_program::{
        borsh::try_from_slice_unchecked,
        hash::Hash,
        instruction::{AccountMeta, Instruction, InstructionError},
        pubkey::Pubkey,
        stake, system_program, sysvar,
    },
    solana_program_test::*,
    solana_sdk::{
        signature::{Keypair, Signer},
        transaction::{Transaction, TransactionError},
        transport::TransportError,
    },
    spl_stake_pool::{error::StakePoolError, find_stake_program_address, id, instruction, state},
};

async fn setup() -> (
    BanksClient,
    Keypair,
    Hash,
    StakePoolAccounts,
    ValidatorStakeAccount,
) {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash, 1)
        .await
        .unwrap();

    let validator_stake = ValidatorStakeAccount::new(&stake_pool_accounts.stake_pool.pubkey(), 0);
    create_vote(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &validator_stake.validator,
        &validator_stake.vote,
    )
    .await;

    (
        banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake,
    )
}

#[tokio::test]
async fn success() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await;
    assert!(error.is_none());

    // Check if validator account was added to the list
    let validator_list = get_account(
        &mut banks_client,
        &stake_pool_accounts.validator_list.pubkey(),
    )
    .await;
    let validator_list =
        try_from_slice_unchecked::<state::ValidatorList>(validator_list.data.as_slice()).unwrap();
    assert_eq!(
        validator_list,
        state::ValidatorList {
            header: state::ValidatorListHeader {
                account_type: state::AccountType::ValidatorList,
                max_validators: stake_pool_accounts.max_validators,
            },
            validators: vec![state::ValidatorStakeInfo {
                status: state::StakeStatus::Active,
                vote_account_address: validator_stake.vote.pubkey(),
                last_update_epoch: 0,
                active_stake_lamports: 0,
                transient_stake_lamports: 0,
                transient_seed_suffix_start: 0,
                transient_seed_suffix_end: 0,
            }]
        }
    );

    // Check stake account existence and authority
    let stake = get_account(&mut banks_client, &validator_stake.stake_account).await;
    let stake_state = deserialize::<stake::state::StakeState>(&stake.data).unwrap();
    match stake_state {
        stake::state::StakeState::Stake(meta, _) => {
            assert_eq!(
                &meta.authorized.staker,
                &stake_pool_accounts.withdraw_authority
            );
            assert_eq!(
                &meta.authorized.withdrawer,
                &stake_pool_accounts.withdraw_authority
            );
        }
        _ => panic!(),
    }
}

#[tokio::test]
async fn fail_with_wrong_validator_list_account() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let wrong_validator_list = Keypair::new();

    let mut transaction = Transaction::new_with_payer(
        &[instruction::add_validator_to_pool(
            &id(),
            &stake_pool_accounts.stake_pool.pubkey(),
            &stake_pool_accounts.staker.pubkey(),
            &payer.pubkey(),
            &stake_pool_accounts.withdraw_authority,
            &wrong_validator_list.pubkey(),
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &stake_pool_accounts.staker], recent_blockhash);
    #[allow(clippy::useless_conversion)] // Remove during upgrade to 1.10
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap()
        .into();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = StakePoolError::InvalidValidatorStakeList as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while try to add validator stake address with wrong validator stake list account"),
    }
}

#[tokio::test]
async fn fail_double_add() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await;

    let latest_blockhash = banks_client.get_recent_blockhash().await.unwrap();

    let transaction_error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &latest_blockhash,
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = StakePoolError::ValidatorAlreadyAdded as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while try to add already added validator stake account"),
    }
}

#[tokio::test]
async fn fail_wrong_staker() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let malicious = Keypair::new();

    let mut transaction = Transaction::new_with_payer(
        &[instruction::add_validator_to_pool(
            &id(),
            &stake_pool_accounts.stake_pool.pubkey(),
            &malicious.pubkey(),
            &payer.pubkey(),
            &stake_pool_accounts.withdraw_authority,
            &stake_pool_accounts.validator_list.pubkey(),
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer, &malicious], recent_blockhash);
    #[allow(clippy::useless_conversion)] // Remove during upgrade to 1.10
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap()
        .into();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = StakePoolError::WrongStaker as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while malicious try to add validator stake account"),
    }
}

#[tokio::test]
async fn fail_without_signature() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let accounts = vec![
        AccountMeta::new(stake_pool_accounts.stake_pool.pubkey(), false),
        AccountMeta::new_readonly(stake_pool_accounts.staker.pubkey(), false),
        AccountMeta::new(payer.pubkey(), false),
        AccountMeta::new_readonly(stake_pool_accounts.withdraw_authority, false),
        AccountMeta::new(stake_pool_accounts.validator_list.pubkey(), false),
        AccountMeta::new(validator_stake.stake_account, false),
        AccountMeta::new(validator_stake.vote.pubkey(), false),
        AccountMeta::new_readonly(sysvar::rent::id(), false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(sysvar::stake_history::id(), false),
        AccountMeta::new_readonly(stake::config::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(stake::program::id(), false),
    ];
    let instruction = Instruction {
        program_id: id(),
        accounts,
        data: instruction::StakePoolInstruction::AddValidatorToPool
            .try_to_vec()
            .unwrap(),
    };

    let mut transaction = Transaction::new_with_payer(&[instruction], Some(&payer.pubkey()));
    transaction.sign(&[&payer], recent_blockhash);
    #[allow(clippy::useless_conversion)] // Remove during upgrade to 1.10
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap()
        .into();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = StakePoolError::SignatureMissing as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while malicious try to add validator stake account without signing transaction"),
    }
}

#[tokio::test]
async fn fail_with_wrong_stake_program_id() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let wrong_stake_program = Pubkey::new_unique();
    let accounts = vec![
        AccountMeta::new(stake_pool_accounts.stake_pool.pubkey(), false),
        AccountMeta::new_readonly(stake_pool_accounts.staker.pubkey(), true),
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(stake_pool_accounts.withdraw_authority, false),
        AccountMeta::new(stake_pool_accounts.validator_list.pubkey(), false),
        AccountMeta::new(validator_stake.stake_account, false),
        AccountMeta::new(validator_stake.vote.pubkey(), false),
        AccountMeta::new_readonly(sysvar::rent::id(), false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(sysvar::stake_history::id(), false),
        AccountMeta::new_readonly(stake::config::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(wrong_stake_program, false),
    ];
    let instruction = Instruction {
        program_id: id(),
        accounts,
        data: instruction::StakePoolInstruction::AddValidatorToPool
            .try_to_vec()
            .unwrap(),
    };
    let mut transaction = Transaction::new_with_payer(&[instruction], Some(&payer.pubkey()));
    transaction.sign(&[&payer, &stake_pool_accounts.staker], recent_blockhash);
    #[allow(clippy::useless_conversion)] // Remove during upgrade to 1.10
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap()
        .into();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(_, error)) => {
            assert_eq!(error, InstructionError::IncorrectProgramId);
        }
        _ => panic!(
            "Wrong error occurs while try to add validator stake account with wrong stake program ID"
        ),
    }
}

#[tokio::test]
async fn fail_with_wrong_system_program_id() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let wrong_system_program = Pubkey::new_unique();

    let accounts = vec![
        AccountMeta::new(stake_pool_accounts.stake_pool.pubkey(), false),
        AccountMeta::new_readonly(stake_pool_accounts.staker.pubkey(), true),
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(stake_pool_accounts.withdraw_authority, false),
        AccountMeta::new(stake_pool_accounts.validator_list.pubkey(), false),
        AccountMeta::new(validator_stake.stake_account, false),
        AccountMeta::new(validator_stake.vote.pubkey(), false),
        AccountMeta::new_readonly(sysvar::rent::id(), false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(sysvar::stake_history::id(), false),
        AccountMeta::new_readonly(stake::config::id(), false),
        AccountMeta::new_readonly(wrong_system_program, false),
        AccountMeta::new_readonly(stake::program::id(), false),
    ];
    let instruction = Instruction {
        program_id: id(),
        accounts,
        data: instruction::StakePoolInstruction::AddValidatorToPool
            .try_to_vec()
            .unwrap(),
    };
    let mut transaction = Transaction::new_with_payer(&[instruction], Some(&payer.pubkey()));
    transaction.sign(&[&payer, &stake_pool_accounts.staker], recent_blockhash);
    #[allow(clippy::useless_conversion)] // Remove during upgrade to 1.10
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap()
        .into();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(_, error)) => {
            assert_eq!(error, InstructionError::IncorrectProgramId);
        }
        _ => panic!(
            "Wrong error occurs while try to add validator stake account with wrong stake program ID"
        ),
    }
}

#[tokio::test]
async fn fail_add_too_many_validator_stake_accounts() {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let mut stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts.max_validators = 1;
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash, 1)
        .await
        .unwrap();

    let validator_stake = ValidatorStakeAccount::new(&stake_pool_accounts.stake_pool.pubkey(), 0);
    create_vote(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &validator_stake.validator,
        &validator_stake.vote,
    )
    .await;

    let error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await;
    assert!(error.is_none());

    let validator_stake = ValidatorStakeAccount::new(&stake_pool_accounts.stake_pool.pubkey(), 0);
    create_vote(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &validator_stake.validator,
        &validator_stake.vote,
    )
    .await;
    let error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &validator_stake.stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        error,
        TransactionError::InstructionError(0, InstructionError::AccountDataTooSmall),
    );
}

#[tokio::test]
async fn fail_with_unupdated_stake_pool() {} // TODO

#[tokio::test]
async fn fail_with_uninitialized_validator_list_account() {} // TODO

#[tokio::test]
async fn fail_on_non_vote_account() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, _) = setup().await;

    let validator = Pubkey::new_unique();
    let (stake_account, _) =
        find_stake_program_address(&id(), &validator, &stake_pool_accounts.stake_pool.pubkey());

    let error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &stake_account,
            &validator,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        error,
        TransactionError::InstructionError(0, InstructionError::IncorrectProgramId,)
    );
}

#[tokio::test]
async fn fail_on_incorrectly_derived_stake_account() {
    let (mut banks_client, payer, recent_blockhash, stake_pool_accounts, validator_stake) =
        setup().await;

    let bad_stake_account = Pubkey::new_unique();
    let error = stake_pool_accounts
        .add_validator_to_pool(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &bad_stake_account,
            &validator_stake.vote.pubkey(),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        error,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(StakePoolError::InvalidStakeAccountAddress as u32),
        )
    );
}
