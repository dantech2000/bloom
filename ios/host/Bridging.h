// The C functions of libbloom_ios.a that App.swift calls.
#import <UIKit/UIKit.h>

void bloom_ios_start(void);
void gpui_ios_set_embedded(void);
void *gpui_ios_get_window(void);
void *gpui_ios_view_controller(void *window);
void gpui_ios_request_frame(void *window);
void gpui_ios_did_become_active(void *app);
void gpui_ios_will_resign_active(void *app);
void gpui_ios_did_enter_background(void *app);
void gpui_ios_will_enter_foreground(void *app);
