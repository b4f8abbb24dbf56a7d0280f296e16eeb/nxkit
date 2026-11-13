#pragma once
#include <stdbool.h>
#include "../../../../crates/nxkit-host/include/nxkit_ios.h"
#ifdef __OBJC__
#import <UIKit/UIKit.h>
NS_ASSUME_NONNULL_BEGIN
// Main-thread only. The scene owns this view for the process-lifetime session.
UIView *nxkit_overlay_create(CGRect frame);
NS_ASSUME_NONNULL_END
#endif
void *nxkit_game_menu(void); // Swift returns a retained UIMenu.
void nxkit_overlay_active(bool active);
void nxkit_overlay_reset(void);
int nxkit_pad_layout(void);
bool nxkit_pad_layout_offered(int layout);
void nxkit_pad_layout_set(int layout);
bool nxkit_pad_feedback(void);
void nxkit_pad_feedback_set(bool on);
