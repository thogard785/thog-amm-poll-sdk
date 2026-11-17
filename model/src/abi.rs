//! The parts of the pool's ABI an aggregator uses, as [`alloy_sol_types`] bindings.
//!
//! Encode a call with [`SolCall::abi_encode`], for example
//! `abi::makerSwapExactInputCall { .. }.abi_encode()`. The complete JSON ABI is
//! in the integration guide: <https://github.com/thogard785/thog-amm-integration-guide/blob/main/abi/ThogAMM.json>
use alloy_sol_types::sol;
/// The traits that encode and decode the types below, re-exported so a caller
/// needs no dependency of its own.
pub use alloy_sol_types::{SolCall, SolEvent, SolValue};

sol! {
    // ---- Swaps and quotes -------------------------------------------------

    function makerQuoteExactInput(address tokenIn, address tokenOut, uint256 amountIn)
        external view returns (uint256 amountOut, uint256 lastPostedBlock);
    function makerSwapExactInput(address tokenIn, address tokenOut, uint256 amountIn,
        uint256 minAmountOut, address recipient, uint256 deadlineBlock) external returns (uint256 amountOut);
    function swapWithExtraFees(address tokenIn, address tokenOut, uint256 amountIn,
        uint256 minAmountOut, uint256 expiry, uint256 extraFeePpm, address feeRecipient)
        external returns (uint256 amountOut);

    // ---- ERC-7815 (https://github.com/ethereum/ERCs/blob/70e96af4539a39ed2bf564ec3559fb45159ff223/ERCS/erc-7815.md) ---------------

    #[derive(Debug, PartialEq, Eq)]
    struct Fraction { uint256 numerator; uint256 denominator; }
    #[derive(Debug, PartialEq, Eq)]
    struct Trade { uint256 calculatedAmount; uint256 gasUsed; Fraction marginalPriceAfterTrade; }
    #[derive(Debug, PartialEq, Eq)]
    enum OrderSide { Sell, Buy }

    function getPoolIds() external pure returns (bytes32[] memory poolIds);
    function getTokens(bytes32 poolId) external view returns (address[] memory tokens);
    function getLimits(bytes32 poolId, address sellToken, address buyToken)
        external view returns (uint256[] memory limits);
    function price(bytes32 poolId, address sellToken, address buyToken, uint256[] memory specifiedAmounts)
        external view returns (Fraction[] memory prices);
    function swap(bytes32 poolId, address sellToken, address buyToken, OrderSide side, uint256 specifiedAmount)
        external returns (Trade memory trade);

    error Unavailable(string reason);
    error LimitExceeded(uint256 limit);
    error NotImplemented(string reason);
    error MakerAmountTooLarge(uint256 limit);
    error MakerFeeTokenSetupFailed();
    error MakerFeeConfirmationFailed();

    // ---- Pool state -------------------------------------------------------

    /// A field reference is `slot << 16 | bitOffset << 8 | bitWidth`; see `layout::Field::reference`.
    #[derive(Debug, PartialEq, Eq)]
    struct TokenMetadata {
        address token;
        uint8 decimals;
        uint8 category;
        uint32 price;
        uint32 priceTail;
        uint32 tokenBidSpread;
        uint32 tokenAskSpread;
        uint32 volumeRate;
        uint32 categorySpread;
        uint32 inventory;
        uint32 sides;
        uint32 postedBlock;
        uint32 sequence;
        uint32 pairStart;
        uint32 inventoryCap;
    }

    #[derive(Debug, PartialEq, Eq)]
    struct CategoryMetadata {
        uint8 anchorTokenIndex;
        uint32 price;
        uint32 priceTail;
        uint32 inventory;
        uint32 spread;
        uint32 postedBlock;
        uint32 inventoryCap;
    }

    #[derive(Debug, PartialEq, Eq)]
    struct StorageWord {
        uint16 slot;
        uint256 value;
    }

    #[derive(Debug, PartialEq, Eq)]
    struct PoolData {
        uint16 schemaVersion;
        uint256 blockNumber;
        uint256 baseFeePerGas;
        bytes32 parentBlockHash;
        uint8 tokenCount;
        uint8 categoryCount;
        uint8 startTokenIndex;
        TokenMetadata[] tokens;
        uint256[] balances;
        CategoryMetadata[] categories;
        StorageWord[] words;
        uint256 nativeBalance;
        uint256[] portfolioBalances;
    }

    /// `getPoolData(0, 28)` returns every listed token.
    function getPoolData(uint8 startTokenIndex, uint8 stopTokenIndex) external view returns (PoolData memory data);
    /// Zero for an ordinary token; the multiplier the pool prices an xStock with otherwise.
    function storedMultiplier(address token) external view returns (uint256 multiplier);
    /// On an xStock token contract: the holder's share balance.
    function sharesOf(address holder) external view returns (uint256 shares);

    // ---- Events that change pool state -------------------------------------

    event MakerPageUpdated(uint8 indexed lane, bytes payload, uint256 control);
    event MakerLendingExposureUpdated(uint8 indexed venue, uint8 indexed category, uint32 sequence, int64 offset);
    event MakerInventoryUpdated(uint8 indexed index, address indexed token, uint128 balance);
    event MakerMultiplierUpdated(address indexed token, uint256 multiplier);
    event MakerWmonBalanceCheckpoint(uint256 balance);
    event NativeBalanceCheckpoint(uint256 balance);
    event MakerTokenAdded(uint8 indexed tokenIndex, address indexed token, TokenMetadata metadata, CategoryMetadata categoryMetadata, uint256 balance);
    event MakerTokenRetired(uint8 indexed retiredIndex, address indexed token, uint8 movedFrom, uint32 generation);
    event Paused(bool paused);
    event Upgraded(address indexed implementation);

    // ---- Token events ------------------------------------------------------

    event Transfer(address indexed from, address indexed to, uint256 value);
    event TransferShares(address indexed from, address indexed to, uint256 value);
    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);

    // ---- Multicall3 (https://www.multicall3.com) ---------------------------

    struct Call3 { address target; bool allowFailure; bytes callData; }
    struct CallResult { bool success; bytes returnData; }
    function aggregate3(Call3[] calldata calls) external payable returns (CallResult[] memory returnData);
}
