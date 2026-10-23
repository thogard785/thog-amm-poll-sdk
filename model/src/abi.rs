//! Exact schema-7 ABI of the deployed ThogAMM proxy.
use alloy_sol_types::sol;
sol! {
    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        uint32 inventoryCap; // uint24: raw-unit mantissa:16 | binary shift:8; all-one = uint256.max
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct CategoryMetadata {
        uint8 anchorTokenIndex;
        uint32 price;
        uint32 priceTail;
        uint32 inventory;
        uint32 spread;
        uint32 postedBlock;
        uint32 inventoryCap; // same encoding, denominated in USD wei
    }

    // Decoded word: the physical 1<<255 empty marker is returned as zero.
    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct StorageWord {
        uint16 slot;
        uint256 value;
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct PoolData {
        uint16 schemaVersion;
        uint256 blockNumber;
        uint256 baseFeePerGas;
        // The EVM cannot read the current block's hash. This anchors its parent.
        bytes32 parentBlockHash;
        uint8 tokenCount;
        uint8 categoryCount;
        uint8 startTokenIndex;
        TokenMetadata[] tokens;
        uint256[] balances;
        // Every category is included: quotes depend on the entire portfolio.
        CategoryMetadata[] categories;
        // All 126 quote and configuration words (slots 2..127), regardless of pagination.
        StorageWord[] words;
        // Native MON contributes to WMON category risk, not ERC-20 capacity.
        uint256 nativeBalance;
        // Every registered ERC-20 balance in global token-index order.
        uint256[] portfolioBalances;
    }


    function getPoolData(uint8 startTokenIndex, uint8 stopTokenIndex) external view returns (PoolData data);
    function maxIndex() external view returns (uint8 index);
    function makerSwapExactInput(address tokenIn, address tokenOut, uint256 amountIn,
        uint256 minAmountOut, address recipient, uint256 deadlineBlock) external returns (uint256 amountOut);
    event MakerPageUpdated(uint8 indexed lane, bytes payload, uint256 control);
    event MakerLendingExposureUpdated(uint8 indexed venue, uint8 indexed category, uint32 sequence, int64 offset);
    event MakerTokenAdded(uint8 indexed tokenIndex, address indexed token, TokenMetadata metadata, CategoryMetadata categoryMetadata, uint256 balance);
    event MakerTokenRetired(uint8 indexed retiredIndex, address indexed token, uint8 movedFrom, uint32 generation);
    event MakerWmonBalanceCheckpoint(uint256 balance);
    event NativeBalanceCheckpoint(uint256 balance);
    event Paused(bool paused);
    event Upgraded(address indexed implementation);
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);
}
