//! `nn::ec` take-overs: the shop applet pages, the purchased-item queue and the shop service accessor. Source: nxnative
//! `dm/host/hle_misc.c` (the ec section) and `dm/host/hle_sdk16.c` (`ShowShopApplicationInformationForUpgrade`).
//!
//! The host has no shop. A `ShowShop*` call opens the library applet for the time of the call, logs it and returns, as
//! if the player closed the shop at once. The purchased-item queue is empty and every watcher gets the same
//! unsignalled event for the life of the process. The shop service accessor never initializes, so no request is sent.
//! A response is complete at once and holds nothing. The SDK destructor of the accessor calls `Finalize` and aborts
//! unless it succeeds, so the whole accessor is taken over.

use super::oe_state::AppletGuard;
use super::{fail, log, nothing, one, zero, zero64};
use crate::event::{self, Block};

static PURCHASED_EVENT: Block = Block::new();

/// Makes an export that opens the shop applet for the call and logs `what`.
macro_rules! shop_page {
    ($name:ident, $what:literal) => {
        extern "C" fn $name() {
            let _applet = AppletGuard::new("shop");
            log(concat!("ec::", $what, " (cut): the host has no shop"));
        }
    };
}

shop_page!(add_on_content_list, "ShowShopAddOnContentList");
shop_page!(application_information, "ShowShopApplicationInformation");
shop_page!(application_upgrade, "ShowShopApplicationInformationForUpgrade");
shop_page!(subscription_list, "ShowShopSubscriptionList");
shop_page!(consumable_list, "ShowShopConsumableItemList");
shop_page!(consumable_detail, "ShowShopConsumableItemDetail");
shop_page!(enter_code, "ShowShopEnterCodeScene");
shop_page!(product_details, "ShowShopProductDetails");
shop_page!(product_list, "ShowShopProductList");
shop_page!(subscription_details, "ShowShopSubscriptionDetails");

/// `GetPurchasedEvent()`: the event of the empty queue.
extern "C" fn purchased_event() -> u64 {
    let ev = PURCHASED_EVENT.addr(0x80);
    event::bind(ev, false);
    ev
}

/// `AsyncResponse::GetSize(size_t*)`: no data.
extern "C" fn response_size(_this: u64, n: u64) -> u32 {
    super::put_u64(n, 0);
    super::RES_FAIL
}

table! {
    "_ZN2nn2ec24InitializePurchasedEventEv" => nothing,
    "_ZN2nn2ec27InitializeAllPurchasedEventEv" => nothing,
    "_ZN2nn2ec27InitializeAllPurchasedEventERKNS_13ApplicationIdE" => nothing,
    "_ZN2nn2ec22FinalizePurchasedEventEv" => nothing,
    "_ZN2nn2ec17GetPurchasedEventEv" => purchased_event,
    "_ZN2nn2ec20PopPurchasedItemInfoEPNS0_17PurchasedItemInfoE" => zero,
    "_ZN2nn2ec20PopPurchasedItemInfoEPNS0_27PurchasedItemInfoForPrivateE" => zero,
    "_ZN2nn2ec24ShowShopAddOnContentListEv" => add_on_content_list,
    "_ZN2nn2ec24ShowShopAddOnContentListENS_13ApplicationIdE" => add_on_content_list,
    "_ZN2nn2ec24ShowShopAddOnContentListENS_13ApplicationIdERKNS_7account10UserHandleE" => add_on_content_list,
    "_ZN2nn2ec24ShowShopAddOnContentListERKNS_7account10UserHandleE" => add_on_content_list,
    "_ZN2nn2ec30ShowShopApplicationInformationEv" => application_information,
    "_ZN2nn2ec30ShowShopApplicationInformationERKNS_7account10UserHandleE" => application_information,
    "_ZN2nn2ec30ShowShopApplicationInformationENS_13ApplicationIdE" => application_information,
    "_ZN2nn2ec30ShowShopApplicationInformationENS_13ApplicationIdERKNS_7account10UserHandleE" => application_information,
    "_ZN2nn2ec40ShowShopApplicationInformationForUpgradeENS0_14UpgradeEditionE" => application_upgrade,
    "_ZN2nn2ec40ShowShopApplicationInformationForUpgradeENS0_14UpgradeEditionERKNS_7account10UserHandleE" => application_upgrade,
    "_ZN2nn2ec24ShowShopSubscriptionListEv" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListERKNS_7account10UserHandleE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListENS_13ApplicationIdE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListENS_13ApplicationIdERKNS_7account10UserHandleE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListENS_13ApplicationIdERKNS0_8CourseIdE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListENS_13ApplicationIdERKNS0_8CourseIdERKNS_7account10UserHandleE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListERKNS0_8CourseIdE" => subscription_list,
    "_ZN2nn2ec24ShowShopSubscriptionListERKNS0_8CourseIdERKNS_7account10UserHandleE" => subscription_list,
    "_ZN2nn2ec26ShowShopConsumableItemListEv" => consumable_list,
    "_ZN2nn2ec26ShowShopConsumableItemListERKNS_7account10UserHandleE" => consumable_list,
    "_ZN2nn2ec26ShowShopConsumableItemListENS_13ApplicationIdE" => consumable_list,
    "_ZN2nn2ec26ShowShopConsumableItemListENS_13ApplicationIdERKNS_7account10UserHandleE" => consumable_list,
    "_ZN2nn2ec28ShowShopConsumableItemDetailERKNS0_12ConsumableIdENS0_5NsUidE" => consumable_detail,
    "_ZN2nn2ec28ShowShopConsumableItemDetailERKNS0_12ConsumableIdENS0_5NsUidERKNS_7account10UserHandleE" => consumable_detail,
    "_ZN2nn2ec28ShowShopConsumableItemDetailENS_13ApplicationIdERKNS0_12ConsumableIdENS0_5NsUidE" => consumable_detail,
    "_ZN2nn2ec28ShowShopConsumableItemDetailENS_13ApplicationIdERKNS0_12ConsumableIdENS0_5NsUidERKNS_7account10UserHandleE" => consumable_detail,
    "_ZN2nn2ec22ShowShopEnterCodeSceneEv" => enter_code,
    "_ZN2nn2ec22ShowShopEnterCodeSceneERKNS_7account10UserHandleE" => enter_code,
    "_ZN2nn2ec22ShowShopProductDetailsENS0_5NsUidE" => product_details,
    "_ZN2nn2ec22ShowShopProductDetailsENS0_5NsUidERKNS_7account10UserHandleE" => product_details,
    "_ZN2nn2ec19ShowShopProductListEPKNS0_5NsUidEiPKc" => product_list,
    "_ZN2nn2ec19ShowShopProductListEPKNS0_5NsUidEiPKcRKNS_7account10UserHandleE" => product_list,
    "_ZN2nn2ec27ShowShopSubscriptionDetailsERKNS0_8CourseIdENS0_5NsUidERKNS_7account10UserHandleE" => subscription_details,
    "_ZN2nn2ec33InitializeForShopServiceAccessorsEPvm" => zero,
    "_ZN2nn2ec31FinalizeForShopServiceAccessorsEv" => nothing,
    "_ZN2nn2ec19ShopServiceAccessorC1Ev" => nothing,
    "_ZN2nn2ec19ShopServiceAccessorD1Ev" => nothing,
    "_ZN2nn2ec19ShopServiceAccessor10InitializeERKNS0_11ShopServiceE" => fail,
    "_ZN2nn2ec19ShopServiceAccessor7RequestEPNS1_13AsyncResponseERKNS_7account3UidENS0_11ShopService6MethodEPKcRKNS1_8PostDataENS_8TimeSpanE" => fail,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponseC1Ev" => nothing,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponseD1Ev" => nothing,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponse7TryWaitEv" => one,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponse7GetSizeEPm" => response_size,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponse3GetEPvm" => fail,
    "_ZN2nn2ec19ShopServiceAccessor13AsyncResponse6CancelEv" => nothing,
    "_ZNK2nn2ec19ShopServiceAccessor13AsyncResponse12GetErrorCodeEv" => zero64,
}
