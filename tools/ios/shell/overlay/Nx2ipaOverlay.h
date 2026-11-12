#pragma once
#include <stdbool.h>
#include "../../../../crates/nxkit-host/include/nxkit_ios.h"
#ifdef __OBJC__
#import <UIKit/UIKit.h>
NS_ASSUME_NONNULL_BEGIN
// Main-thread only. The scene owns this view for the process-lifetime session.
UIView *nx2ipa_overlay_create(CGRect frame);
NS_ASSUME_NONNULL_END
#endif
void *nx2ipa_game_menu(void); // Swift returns a retained UIMenu.
void nx2ipa_overlay_active(bool active);
void nx2ipa_overlay_reset(void);
int nx2ipa_pad_layout(void);
bool nx2ipa_pad_layout_offered(int layout);
void nx2ipa_pad_layout_set(int layout);
bool nx2ipa_pad_feedback(void);
void nx2ipa_pad_feedback_set(bool on);
