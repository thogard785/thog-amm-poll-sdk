//! Typed ABI for schema 6. Layouts and event signatures match the Solidity sources.
use alloy_sol_types::sol;
sol! {
    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct TokenMetadata {
        address token;
        uint8 decimals;
        uint8 category;
        uint32 price;
        uint32 priceTail;
        uint32 tokenSpread;
        uint32 categorySpread;
        uint32 inventory;
        uint32 sides;
        uint32 postedBlock;
        uint32 sequence;
        uint32 pairStart;
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct CategoryMetadata {
        uint8 anchorTokenIndex;
        uint32 price;
        uint32 priceTail;
        uint32 inventory;
        uint32 spread;
        uint32 postedBlock;
    }

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
        bytes32 parentBlockHash;
        uint8 tokenCount;
        uint8 categoryCount;
        uint8 startTokenIndex;
        TokenMetadata[] tokens;
        uint256[] balances;
        // Every category is included: quotes depend on the entire portfolio.
        CategoryMetadata[] categories;
        // Sorted, unique physical words, including shared risk dependencies.
        StorageWord[] words;
    }


    function getPoolData(uint8 startTokenIndex, uint8 stopTokenIndex) external view returns (PoolData data);
    function maxIndex() external view returns (uint8 index);
    // Direct exact-input settlement, with raw-unit min output and block-number
    // deadline. This binding only encodes calldata; it never sends a transaction.
    function makerSwapExactInput(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        address recipient,
        uint256 deadlineBlock
    ) external returns (uint256 amountOut);
    event MakerTokenAdded(uint8 indexed tokenIndex, address indexed token, TokenMetadata metadata, CategoryMetadata categoryMetadata, uint256 balance);
    event MakerStorageUpdated(uint16 indexed slot, uint256 value);
    event MakerPriceStateUpdated(uint256 v1, uint256 auxiliary);
    /// @notice Full post-write risk state for event-driven local quote mirrors.
    /// @dev V1 is the raw post-write word, including the internal global-enable
    /// bit. `fields` preserves which subset was intentionally changed.
    event MakerRiskStateUpdated(uint8 indexed fields, uint256 v1, uint256 r1, uint256 r2, uint256 i1);
    /// @notice Complete five-category risk transition for V3-aware mirrors.
    event MakerRiskStateUpdatedV3(
        uint8 indexed fields, uint256 v1, uint256 v3, uint256 r1, uint256 r2, uint256 r3, uint256 i1, uint256 i2
    );
    /// @notice Complete pair-calibrated risk transition for canonical mirrors.
    /// @dev P1 and P2 hold all 28 unordered token-pair competition widths.
    event MakerRiskStateUpdatedV4(
        uint8 indexed fields,
        uint256 v1,
        uint256 v3,
        uint256 r1,
        uint256 r2,
        uint256 r3,
        uint256 i1,
        uint256 i2,
        uint256 p1,
        uint256 p2
    );
    /// @notice Exact post-fill inventory word after a cross-category transition.
    event MakerInventoryUpdated(uint256 i1);
    /// @notice Complete post-fill inventory after a transition involving XAUT.
    event MakerInventoryUpdatedV3(uint256 i1, uint256 i2);


    event MakerWmonBalanceCheckpoint(uint256 balance);
    event Paused(bool paused);
    event Upgraded(address indexed implementation);
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);
}
