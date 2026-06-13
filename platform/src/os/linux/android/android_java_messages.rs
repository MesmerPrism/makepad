use super::*;

impl Cx {
    fn is_pure_touch_move_message(msg: &FromJavaMessage) -> bool {
        if let FromJavaMessage::Touch(touches) = msg {
            touches
                .iter()
                .all(|t| t.state == crate::event::finger::TouchState::Move)
        } else {
            false
        }
    }

    fn is_video_hardware_buffer_stereo_frame_message(msg: &FromJavaMessage) -> bool {
        matches!(
            msg,
            FromJavaMessage::VideoHardwareBufferStereoFrame { .. }
                | FromJavaMessage::VideoHardwareBufferStereoFrameReady { .. }
        )
    }

    fn release_coalesced_video_hardware_buffer_stereo_message(msg: FromJavaMessage) {
        if let FromJavaMessage::VideoHardwareBufferStereoFrame {
            left_hardware_buffer,
            right_hardware_buffer,
            ..
        } = msg
        {
            unsafe {
                ndk_sys::AHardwareBuffer_release(left_hardware_buffer);
                ndk_sys::AHardwareBuffer_release(right_hardware_buffer);
            }
        }
    }

    pub(crate) fn handle_coalesced_android_java_message(
        &mut self,
        phase: &str,
        msg: FromJavaMessage,
        pending_touch_move: &mut Option<FromJavaMessage>,
        pending_stereo_hardware_buffer_frame: &mut Option<FromJavaMessage>,
        dropped_stereo_hardware_buffer_frames: &mut u64,
    ) {
        if Self::is_pure_touch_move_message(&msg) {
            *pending_touch_move = Some(msg);
            return;
        }
        if Self::is_video_hardware_buffer_stereo_frame_message(&msg) {
            if let Some(previous) = pending_stereo_hardware_buffer_frame.replace(msg) {
                Self::release_coalesced_video_hardware_buffer_stereo_message(previous);
                *dropped_stereo_hardware_buffer_frames =
                    dropped_stereo_hardware_buffer_frames.saturating_add(1);
            }
            return;
        }
        self.flush_coalesced_android_java_messages(
            phase,
            pending_touch_move,
            pending_stereo_hardware_buffer_frame,
            dropped_stereo_hardware_buffer_frames,
        );
        self.handle_message(msg);
    }

    pub(crate) fn flush_coalesced_android_java_messages(
        &mut self,
        phase: &str,
        pending_touch_move: &mut Option<FromJavaMessage>,
        pending_stereo_hardware_buffer_frame: &mut Option<FromJavaMessage>,
        dropped_stereo_hardware_buffer_frames: &mut u64,
    ) {
        if let Some(deferred) = pending_touch_move.take() {
            self.handle_message(deferred);
        }
        let kept_stereo_hardware_buffer_frame =
            if let Some(deferred) = pending_stereo_hardware_buffer_frame.take() {
                self.handle_message(deferred);
                true
            } else {
                false
            };
        if *dropped_stereo_hardware_buffer_frames > 0 {
            crate::log!(
                "RUSTY_XR_MAKEPAD_BROKER_H264_STEREO_HARDWARE_BUFFER_COALESCE schema=rusty.xr.makepad-broker-h264-stereo-hardware-buffer-coalesce.v1 phase={} status=drop-old dropped={} kept={} policy=latest-pair-per-drain",
                phase,
                *dropped_stereo_hardware_buffer_frames,
                if kept_stereo_hardware_buffer_frame { 1 } else { 0 },
            );
            *dropped_stereo_hardware_buffer_frames = 0;
        }
    }

    pub(crate) fn handle_message(&mut self, msg: FromJavaMessage) {
        if Self::is_android_surface_message(&msg) {
            self.handle_android_surface_message(msg);
        } else if Self::is_android_input_message(&msg) {
            self.handle_android_input_message(msg);
        } else if Self::is_android_network_message(&msg) {
            self.handle_android_network_message(msg);
        } else if Self::is_android_video_message(&msg) {
            self.handle_android_video_message(msg);
        } else {
            self.handle_android_lifecycle_message(msg);
        }
    }

    fn is_android_surface_message(msg: &FromJavaMessage) -> bool {
        matches!(
            msg,
            FromJavaMessage::SurfaceCreated { .. }
                | FromJavaMessage::SurfaceDestroyed { .. }
                | FromJavaMessage::SurfaceChanged { .. }
                | FromJavaMessage::SafeAreaInsets { .. }
        )
    }

    fn handle_android_surface_message(&mut self, msg: FromJavaMessage) {
        match msg {
            FromJavaMessage::SurfaceCreated { window } => {
                #[cfg(use_vulkan)]
                let _has_vulkan = self.os.vulkan.is_some();
                #[cfg(not(use_vulkan))]
                let _has_vulkan = false;
                #[cfg(not(use_vulkan))]
                if !self.os.in_xr_mode {
                    unsafe {
                        self.os.display.as_mut().unwrap().update_surface(window);
                    }
                }

                #[cfg(use_vulkan)]
                {
                    if let Some(display) = self.os.display.as_mut() {
                        unsafe {
                            if !display.window.is_null() && display.window != window {
                                ndk_sys::ANativeWindow_release(display.window);
                            }
                        }
                        display.window = window;
                    }
                    if !self.os.in_xr_mode {
                        if let Some(vulkan) = self.os.vulkan.as_mut() {
                            let width = self.os.display_size.x.max(1.0) as u32;
                            let height = self.os.display_size.y.max(1.0) as u32;
                            if let Err(err) = vulkan.update_surface(window, width, height) {
                                crate::error!("Android Vulkan surface create/update failed: {err}");
                            }
                        }
                    }
                }

                if !self.os.in_xr_mode {
                    self.sync_android_surface_alive_from_backend();
                    if self.os.surface_alive {
                        self.request_android_surface_redraw();
                    }
                }
            }
            FromJavaMessage::SurfaceDestroyed { ack } => {
                // CRITICAL: clear `surface_alive` BEFORE tearing down the
                // surface itself. The render thread is the only one allowed to
                // touch GL state, and we are on the render thread right now —
                // but the helper functions we call below (`destroy_surface`,
                // `suspend_surface`) issue EGL/Vulkan calls that can themselves
                // trip the renderer if it observes a half-torn-down state.
                self.os.surface_alive = false;
                // Ensure the next time the surface becomes drawable, we force
                // a full redraw to avoid a black screen.
                self.os.needs_first_draw = true;
                // The Java host shows its placeholder cover before sending
                // SurfaceDestroyed, so only arm the hide-on-present path for
                // genuine surface teardown/rebuild cycles, not cold start.
                self.os.hide_surface_cover_after_first_present = true;
                self.os.refresh_surface_snapshot_after_first_present = true;

                #[cfg(not(use_vulkan))]
                unsafe {
                    self.os.display.as_mut().unwrap().destroy_surface();
                }

                #[cfg(use_vulkan)]
                {
                    if let Some(display) = self.os.display.as_mut() {
                        unsafe {
                            if !display.window.is_null() {
                                ndk_sys::ANativeWindow_release(display.window);
                                display.window = std::ptr::null_mut();
                            }
                        }
                    }
                    let keep_xr_backend_alive =
                        self.os.in_xr_mode && self.os.openxr.session.is_some();
                    if !keep_xr_backend_alive {
                        if let Some(vulkan) = self.os.vulkan.as_mut() {
                            vulkan.suspend_surface();
                        }
                    }
                    if self.os.in_xr_mode
                        && self.os.openxr.session.is_none()
                        && self.os.xr_retry_surface_after_destroy
                        && !self.os.xr_pending_surface_window.is_null()
                    {
                        crate::log!(
                            "Android XR retrying session creation after SurfaceDestroyed with stored surface {:?} size={}x{}",
                            self.os.xr_pending_surface_window,
                            self.os.xr_pending_surface_width,
                            self.os.xr_pending_surface_height
                        );
                        self.try_create_xr_session_for_surface(
                            self.os.xr_pending_surface_window,
                            self.os.xr_pending_surface_width,
                            self.os.xr_pending_surface_height,
                            "surface-destroyed-retry",
                        );
                    }
                }

                // Tell the JNI thread (which is blocked inside
                // `surfaceOnSurfaceDestroyed`) that it's now safe to return to
                // Android — we've fully released our hold on the surface.
                signal_surface_ack(&ack);
            }
            FromJavaMessage::SurfaceChanged {
                window,
                width,
                height,
            } => {
                #[cfg(use_vulkan)]
                let _has_vulkan = self.os.vulkan.is_some();
                #[cfg(not(use_vulkan))]
                let _has_vulkan = false;
                #[cfg(use_vulkan)]
                if self.os.in_xr_mode {
                    self.replace_xr_pending_surface(window, width, height);
                }
                if self.os.in_xr_mode && self.os.openxr.session.is_none() {
                    #[cfg(use_vulkan)]
                    {
                        self.try_create_xr_session_for_surface(
                            window,
                            width,
                            height,
                            "surface-changed",
                        );
                    }

                    #[cfg(not(use_vulkan))]
                    {
                        if let Err(e) = self.os.openxr.create_session(
                            self.os.display.as_ref().unwrap(),
                            self.current_android_xr_options(),
                            &self.os_type,
                        ) {
                            crate::error!("OpenXR create_xr_session failed: {}", e);
                        }
                    }
                }

                #[cfg(not(use_vulkan))]
                if !self.os.in_xr_mode {
                    unsafe {
                        self.os.display.as_mut().unwrap().update_surface(window);
                    }
                }

                #[cfg(use_vulkan)]
                {
                    if let Some(display) = self.os.display.as_mut() {
                        unsafe {
                            if !display.window.is_null() && display.window != window {
                                ndk_sys::ANativeWindow_release(display.window);
                            }
                        }
                        display.window = window;
                    }
                }

                #[cfg(use_vulkan)]
                {
                    if !self.os.in_xr_mode {
                        let width_u32 = width.max(1) as u32;
                        let height_u32 = height.max(1) as u32;
                        if let Some(vulkan) = self.os.vulkan.as_mut() {
                            if let Err(err) = vulkan.update_surface(window, width_u32, height_u32) {
                                crate::error!("Android Vulkan surface update failed: {err}");
                            }
                        } else {
                            match CxVulkan::new(window, width_u32, height_u32) {
                                Ok(vulkan) => {
                                    self.os.vulkan = Some(vulkan);
                                }
                                Err(err) => {
                                    crate::error!(
                                        "Android Vulkan backend init failed, falling back to OpenGL: {err}"
                                    );
                                }
                            }
                        }
                    }
                }

                if !self.os.in_xr_mode {
                    self.sync_android_surface_alive_from_backend();
                    if self.os.surface_alive {
                        self.request_android_surface_redraw();
                    }
                }

                self.os.display_size = dvec2(width as f64, height as f64);
                let window_id = CxWindowPool::id_zero();
                let window = &mut self.windows[window_id];
                // Stash the OS-reported scale factor so a later
                // `set_window_dpi_override(None)` can recover the native scale,
                // and so `remap_dpi_override` (used by `dpi_override_scale`
                // on platforms whose touch coords aren't already in
                // override-points) has a baseline. Android itself converts
                // touch coords at the source, so the helper is a no-op here.
                window.os_dpi_factor = Some(self.os.dpi_factor);
                let old_geom = window.window_geom.clone();

                let dpi_factor = window.effective_dpi_factor();
                let size = window.physical_vec2d_to_layout(self.os.display_size);
                window.window_geom = WindowGeom {
                    dpi_factor,
                    can_fullscreen: false,
                    xr_is_presenting: false,
                    is_fullscreen: true,
                    is_topmost: true,
                    position: dvec2(0.0, 0.0),
                    inner_size: size,
                    outer_size: size,
                    safe_area_insets: window
                        .native_safe_area_insets_to_layout(self.os.native_safe_area_insets),
                    ..Default::default()
                };
                let new_geom = window.window_geom.clone();
                self.call_event_handler(&Event::WindowGeomChange(WindowGeomChangeEvent {
                    window_id,
                    new_geom,
                    old_geom,
                }));
                if let Some(main_pass_id) = self.windows[window_id].main_pass_id {
                    self.redraw_pass_and_child_passes(main_pass_id);
                }
                self.redraw_all();
                self.os.first_after_resize = true;
                self.call_event_handler(&Event::ClearAtlasses);
            }
            FromJavaMessage::SafeAreaInsets {
                top,
                right,
                bottom,
                left,
            } => {
                let new_insets = crate::event::SafeAreaInsets {
                    top,
                    right,
                    bottom,
                    left,
                };
                if self.os.native_safe_area_insets != new_insets {
                    self.os.native_safe_area_insets = new_insets;
                    // Update the WindowGeom with the new safe area insets
                    let window_id = CxWindowPool::id_zero();
                    let window = &mut self.windows[window_id];
                    let old_geom = window.window_geom.clone();
                    let safe_area_insets = window.native_safe_area_insets_to_layout(new_insets);
                    window.window_geom.safe_area_insets = safe_area_insets;
                    let new_geom = window.window_geom.clone();
                    if old_geom != new_geom {
                        self.call_event_handler(&Event::WindowGeomChange(WindowGeomChangeEvent {
                            window_id,
                            new_geom,
                            old_geom,
                        }));
                        self.redraw_all();
                    }
                }
            }
            _ => unreachable!("non-surface Android Java message routed to surface handler"),
        }
    }

    fn is_android_input_message(msg: &FromJavaMessage) -> bool {
        matches!(
            msg,
            FromJavaMessage::BackPressed
                | FromJavaMessage::LongClick { .. }
                | FromJavaMessage::Touch(_)
                | FromJavaMessage::Character { .. }
                | FromJavaMessage::KeyDown { .. }
                | FromJavaMessage::KeyUp { .. }
                | FromJavaMessage::ResizeTextIME { .. }
                | FromJavaMessage::ClipboardAction { .. }
                | FromJavaMessage::ClipboardPaste { .. }
                | FromJavaMessage::SelectionHandleDrag { .. }
                | FromJavaMessage::ImeTextStateChanged { .. }
                | FromJavaMessage::ImeEditorAction { .. }
        )
    }

    fn handle_android_input_message(&mut self, msg: FromJavaMessage) {
        match msg {
            FromJavaMessage::BackPressed => {
                self.call_event_handler(&Event::BackPressed {
                    handled: Cell::new(false),
                });
            }
            FromJavaMessage::LongClick {
                abs,
                pointer_id,
                time,
            } => {
                let window = &self.windows[CxWindowPool::id_zero()];
                let e = Event::LongPress(LongPressEvent {
                    abs: window.physical_vec2d_to_layout(abs),
                    uid: pointer_id,
                    window_id: CxWindowPool::id_zero(),
                    time,
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::Touch(mut touches) => {
                let time = touches[0].time;
                let window = &self.windows[CxWindowPool::id_zero()];
                for touch in &mut touches {
                    touch.abs = window.physical_vec2d_to_layout(touch.abs);
                    touch.radius = window.physical_vec2d_to_layout(touch.radius);
                }

                // Check for outside-click popup dismiss on touch start
                if touches
                    .iter()
                    .any(|t| t.state == crate::event::finger::TouchState::Start)
                {
                    if let Some(popup_window_id) = self.find_popup_to_dismiss_on_touch(&touches) {
                        self.dismiss_popup_window(
                            popup_window_id,
                            crate::event::PopupDismissReason::OutsideClick,
                        );
                    }
                }

                self.fingers.process_touch_update_start(time, &touches);
                let e = Event::TouchUpdate(TouchUpdateEvent {
                    time,
                    window_id: CxWindowPool::id_zero(),
                    touches,
                    modifiers: Default::default(),
                });
                self.call_event_handler(&e);
                let e = if let Event::TouchUpdate(e) = e {
                    e
                } else {
                    panic!()
                };

                // Synthesize internal drag-and-drop events from touch gestures.
                if self.os.internal_drag_items.is_some() {
                    if let Some(touch) = e
                        .touches
                        .iter()
                        .find(|t| t.state == crate::event::finger::TouchState::Stop)
                    {
                        // Touch lifted: fire Drop + DragEnd
                        if let Some(items) = self.os.internal_drag_items.take() {
                            self.call_event_handler(&Event::Drop(DropEvent {
                                modifiers: e.modifiers.clone(),
                                handled: Arc::new(Mutex::new(false)),
                                abs: touch.abs,
                                items,
                            }));
                            self.drag_drop.cycle_drag();
                            self.call_event_handler(&Event::DragEnd);
                            self.drag_drop.cycle_drag();
                        }
                    } else if let Some(touch) = e
                        .touches
                        .iter()
                        .find(|t| t.state == crate::event::finger::TouchState::Move)
                    {
                        // Finger moving: fire Drag event
                        if let Some(items) = self.os.internal_drag_items.as_ref() {
                            self.call_event_handler(&Event::Drag(DragEvent {
                                modifiers: e.modifiers.clone(),
                                handled: Arc::new(Mutex::new(false)),
                                abs: touch.abs,
                                items: items.clone(),
                                response: Arc::new(Mutex::new(DragResponse::None)),
                            }));
                            self.drag_drop.cycle_drag();
                        }
                    }
                }

                self.fingers.process_touch_update_end(&e.touches);
            }
            FromJavaMessage::Character { character } => {
                if let Some(character) = char::from_u32(character) {
                    let e = Event::TextInput(TextInputEvent {
                        input: character.to_string(),
                        replace_last: false,
                        was_paste: false,
                        ..Default::default()
                    });
                    self.call_event_handler(&e);
                }
            }
            FromJavaMessage::KeyDown {
                keycode,
                meta_state,
            } => {
                let e: Event;
                let makepad_keycode = android_to_makepad_key_code(keycode);
                if !makepad_keycode.is_unknown() {
                    let control = meta_state & ANDROID_META_CTRL_MASK != 0;
                    let alt = meta_state & ANDROID_META_ALT_MASK != 0;
                    let shift = meta_state & ANDROID_META_SHIFT_MASK != 0;
                    let is_shortcut = control || alt;
                    if is_shortcut {
                        if makepad_keycode == KeyCode::KeyC {
                            let response = Rc::new(RefCell::new(None));
                            e = Event::TextCopy(TextClipboardEvent {
                                response: response.clone(),
                            });
                            self.call_event_handler(&e);
                            // let response = response.borrow();
                            // if let Some(response) = response.as_ref(){
                            //     to_java.copy_to_clipboard(response);
                            // }
                        } else if makepad_keycode == KeyCode::KeyX {
                            let response = Rc::new(RefCell::new(None));
                            let e = Event::TextCut(TextClipboardEvent {
                                response: response.clone(),
                            });
                            self.call_event_handler(&e);
                        } else if makepad_keycode == KeyCode::KeyV {
                            let content = unsafe { android_jni::to_java_paste_from_clipboard() };
                            if !content.is_empty() {
                                e = Event::TextInput(TextInputEvent {
                                    input: content,
                                    replace_last: false,
                                    was_paste: true,
                                    ..Default::default()
                                });
                                self.call_event_handler(&e);
                            }
                        }
                    } else {
                        if makepad_keycode == KeyCode::Back {
                            self.call_event_handler(&Event::BackPressed {
                                handled: Cell::new(false),
                            });
                        }

                        e = Event::KeyDown(KeyEvent {
                            key_code: makepad_keycode,
                            is_repeat: false,
                            modifiers: KeyModifiers {
                                shift,
                                control,
                                alt,
                                ..Default::default()
                            },
                            time: self.os.timers.time_now(),
                        });
                        self.call_event_handler(&e);
                    }
                }
            }
            FromJavaMessage::KeyUp {
                keycode,
                meta_state,
            } => {
                let makepad_keycode = android_to_makepad_key_code(keycode);
                let control = meta_state & ANDROID_META_CTRL_MASK != 0;
                let alt = meta_state & ANDROID_META_ALT_MASK != 0;
                let shift = meta_state & ANDROID_META_SHIFT_MASK != 0;

                let e = Event::KeyUp(KeyEvent {
                    key_code: makepad_keycode,
                    is_repeat: false,
                    modifiers: KeyModifiers {
                        shift,
                        control,
                        alt,
                        ..Default::default()
                    },
                    time: self.os.timers.time_now(),
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::ResizeTextIME {
                keyboard_height,
                is_open,
            } => {
                // Java reports the bottom IME occlusion in physical pixels.
                // Convert to Makepad layout points and dedup repeated inset/layout
                // callbacks. A visible IME may still have zero bottom
                // occlusion (floating keyboard, transient animation frame);
                // keep it as a visible zero-height keyboard so KeyboardView can
                // clear any previous bottom shift without treating focus as
                // dismissed.
                let height_logical = self.windows[CxWindowPool::id_zero()]
                    .physical_pixels_to_layout(keyboard_height as f64);
                let time = self.os.timers.time_now();
                if is_open {
                    if self.os.last_ime_visible
                        && (height_logical - self.os.last_ime_height).abs() < 0.5
                    {
                        return;
                    }
                    self.os.last_ime_visible = true;
                    self.os.last_ime_height = height_logical;
                    self.call_event_handler(&Event::VirtualKeyboard(
                        VirtualKeyboardEvent::DidShow {
                            height: height_logical,
                            time,
                        },
                    ))
                } else if !is_open {
                    self.os.last_ime_config = None;
                    if !self.os.last_ime_visible {
                        return;
                    }
                    self.os.last_ime_visible = false;
                    self.os.last_ime_height = 0.0;
                    self.text_ime_was_dismissed();
                    self.call_event_handler(&Event::VirtualKeyboard(
                        VirtualKeyboardEvent::DidHide { time },
                    ))
                }
            }
            FromJavaMessage::ClipboardAction { action } => {
                if action == "copy" {
                    let response = Rc::new(RefCell::new(None));
                    let e = Event::TextCopy(TextClipboardEvent {
                        response: response.clone(),
                    });
                    self.call_event_handler(&e);
                    // Get the copied text from the widget's response
                    if let Some(text) = response.borrow().as_ref() {
                        // Copy to clipboard
                        unsafe {
                            to_java_copy_to_clipboard(text.clone());
                        }
                    };
                } else if action == "cut" {
                    let response = Rc::new(RefCell::new(None));
                    let e = Event::TextCut(TextClipboardEvent {
                        response: response.clone(),
                    });
                    self.call_event_handler(&e);
                    // Get the cut text from the widget's response
                    if let Some(text) = response.borrow().as_ref() {
                        // Copy to clipboard
                        unsafe {
                            to_java_copy_to_clipboard(text.clone());
                        }
                    };
                } else if action == "select_all" {
                    // Simulate Ctrl+A keypress to trigger select_all in widgets
                    let e = Event::KeyDown(KeyEvent {
                        key_code: KeyCode::KeyA,
                        is_repeat: false,
                        modifiers: KeyModifiers {
                            shift: false,
                            control: true, // Ctrl modifier
                            alt: false,
                            logo: false,
                        },
                        time: self.seconds_since_app_start(),
                    });
                    self.call_event_handler(&e);
                }
            }
            FromJavaMessage::ClipboardPaste { content } => {
                let e = Event::TextInput(TextInputEvent {
                    input: content,
                    replace_last: false,
                    was_paste: true,
                    ..Default::default()
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::SelectionHandleDrag {
                handle,
                phase,
                abs,
                time,
            } => {
                let window = &self.windows[CxWindowPool::id_zero()];
                let e = Event::SelectionHandleDrag(SelectionHandleDragEvent {
                    handle,
                    phase,
                    abs: window.physical_vec2d_to_layout(abs),
                    time,
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::ImeTextStateChanged {
                full_text,
                selection_start,
                selection_end,
                composing_start,
                composing_end,
            } => {
                let sel_start = CharOffset::from_utf16_index(&full_text, selection_start as usize);
                let sel_end = CharOffset::from_utf16_index(&full_text, selection_end as usize);

                let composition = if composing_start >= 0 && composing_end >= 0 {
                    let comp_start =
                        CharOffset::from_utf16_index(&full_text, composing_start as usize);
                    let comp_end = CharOffset::from_utf16_index(&full_text, composing_end as usize);
                    Some(comp_start..comp_end)
                } else {
                    None
                };

                let e = Event::TextInput(TextInputEvent {
                    full_state_sync: Some(FullTextState {
                        text: full_text,
                        selection: sel_start..sel_end,
                        composition,
                    }),
                    ..Default::default()
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::ImeEditorAction { action_code } => {
                let action = ImeAction::from_android_action_code(action_code);
                let e = Event::ImeAction(ImeActionEvent { action });
                self.call_event_handler(&e);
            }
            _ => unreachable!("non-input Android Java message routed to input handler"),
        }
    }

    fn is_android_network_message(msg: &FromJavaMessage) -> bool {
        matches!(
            msg,
            FromJavaMessage::HttpResponse { .. }
                | FromJavaMessage::HttpRequestError { .. }
                | FromJavaMessage::WebSocketMessage { .. }
                | FromJavaMessage::WebSocketClosed { .. }
                | FromJavaMessage::WebSocketError { .. }
                | FromJavaMessage::MidiDeviceOpened { .. }
                | FromJavaMessage::PermissionResult { .. }
        )
    }

    fn handle_android_network_message(&mut self, msg: FromJavaMessage) {
        match msg {
            FromJavaMessage::HttpResponse {
                request_id,
                metadata_id,
                status_code,
                headers,
                body,
            } => {
                let out = vec![NetworkResponse::HttpResponse {
                    request_id: LiveId(request_id),
                    response: HttpResponse::from_header_string(
                        LiveId(metadata_id),
                        status_code,
                        headers,
                        Some(body),
                    ),
                }];
                self.handle_script_network_events(&out);
                let e = Event::NetworkResponses(out);
                self.call_event_handler(&e);
            }
            FromJavaMessage::HttpRequestError {
                request_id,
                metadata_id,
                error,
                ..
            } => {
                let out = vec![NetworkResponse::HttpError {
                    request_id: LiveId(request_id),
                    error: HttpError {
                        message: error,
                        metadata_id: LiveId(metadata_id),
                    },
                }];
                self.handle_script_network_events(&out);
                let e = Event::NetworkResponses(out);
                self.call_event_handler(&e);
            }
            FromJavaMessage::WebSocketMessage { message, sender } => {
                let ws_message_parser = self
                    .os
                    .websocket_parsers
                    .entry(sender.0)
                    .or_insert_with(|| WebSocketImpl::new());
                ws_message_parser.parse(&message, |result| match result {
                    Ok(WebSocketMessageImpl::Text(text_msg)) => {
                        let message = WebSocketMessage::String(text_msg.to_string());
                        sender.1.send(message).unwrap();
                    }
                    Ok(WebSocketMessageImpl::Binary(data)) => {
                        let message = WebSocketMessage::Binary(data.to_vec());
                        sender.1.send(message).unwrap();
                    }
                    Err(e) => {
                        println!("Websocket message parse error {:?}", e);
                    }
                    _ => (),
                });
            }
            FromJavaMessage::WebSocketClosed { sender } => {
                self.os.websocket_parsers.remove(&sender.0);
                let message = WebSocketMessage::Closed;
                sender.1.send(message).ok();
            }
            FromJavaMessage::WebSocketError { error, sender } => {
                self.os.websocket_parsers.remove(&sender.0);
                let message = WebSocketMessage::Error(error);
                sender.1.send(message).ok();
            }
            FromJavaMessage::MidiDeviceOpened { name, midi_device } => {
                self.os
                    .media
                    .android_midi()
                    .lock()
                    .unwrap()
                    .midi_device_opened(name, midi_device);
            }
            FromJavaMessage::PermissionResult {
                permission,
                request_id,
                status,
            } => {
                crate::log!(
                    "Android PermissionResult raw permission={} request_id={} status_code={}",
                    permission,
                    request_id,
                    status
                );
                // Convert string permission back to enum
                let perm = string_to_permission(&permission);
                if let Some(perm) = perm {
                    let permission_status = match status {
                        0 => crate::permission::PermissionStatus::NotDetermined,
                        1 => crate::permission::PermissionStatus::Granted,
                        2 => crate::permission::PermissionStatus::DeniedCanRetry,
                        3 => crate::permission::PermissionStatus::DeniedPermanent,
                        _ => {
                            crate::log!("Unknown permission status code: {}", status);
                            crate::permission::PermissionStatus::DeniedPermanent
                            // Default to most restrictive
                        }
                    };

                    self.call_event_handler(&Event::PermissionResult(
                        crate::permission::PermissionResult {
                            permission: perm,
                            request_id,
                            status: permission_status,
                        },
                    ));
                }
            }
            _ => unreachable!("non-network Android Java message routed to network handler"),
        }
    }

    fn is_android_video_message(msg: &FromJavaMessage) -> bool {
        matches!(
            msg,
            FromJavaMessage::VideoPlaybackPrepared { .. }
                | FromJavaMessage::VideoPlaybackMetadata { .. }
                | FromJavaMessage::VideoYuvFrame { .. }
                | FromJavaMessage::VideoHardwareBufferFrame { .. }
                | FromJavaMessage::VideoHardwareBufferStereoFrame { .. }
                | FromJavaMessage::VideoHardwareBufferStereoFrameReady { .. }
                | FromJavaMessage::VideoPlaybackCompleted { .. }
                | FromJavaMessage::VideoPlayerReleased { .. }
                | FromJavaMessage::VideoDecodingError { .. }
                | FromJavaMessage::CameraPreviewSurfaceReady { .. }
                | FromJavaMessage::CameraPreviewSurfaceDestroyed { .. }
        )
    }

    fn handle_android_video_message(&mut self, msg: FromJavaMessage) {
        match msg {
            FromJavaMessage::VideoPlaybackPrepared {
                video_id,
                video_width,
                video_height,
                duration,
                surface_texture,
            } => {
                let e = Event::VideoPlaybackPrepared(VideoPlaybackPreparedEvent {
                    video_id: LiveId(video_id),
                    video_width,
                    video_height,
                    duration,
                    is_seekable: duration > 0,
                    video_tracks: if video_width > 0 && video_height > 0 {
                        vec!["video".to_string()]
                    } else {
                        vec![]
                    },
                    audio_tracks: vec!["audio".to_string()],
                });

                if !surface_texture.is_null() {
                    self.os
                        .video_surfaces
                        .insert(LiveId(video_id), surface_texture);
                }
                self.call_event_handler(&e);
            }
            FromJavaMessage::VideoPlaybackMetadata {
                video_id,
                metadata_json,
            } => {
                let e = Event::VideoPlaybackMetadata(VideoPlaybackMetadataEvent {
                    video_id: LiveId(video_id),
                    metadata_json,
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::VideoYuvFrame {
                video_id,
                width,
                height,
                position_ms,
                y,
                u,
                v,
            } => {
                let live_id = LiveId(video_id);
                if let Some(config) = self.os.video_configs.get(&live_id).cloned() {
                    replace_r8_plane_texture(
                        &mut self.textures,
                        config.tex_y_id,
                        width.max(1) as usize,
                        height.max(1) as usize,
                        y,
                    );
                    replace_r8_plane_texture(
                        &mut self.textures,
                        config.tex_u_id,
                        width.div_ceil(2).max(1) as usize,
                        height.div_ceil(2).max(1) as usize,
                        u,
                    );
                    replace_r8_plane_texture(
                        &mut self.textures,
                        config.tex_v_id,
                        width.div_ceil(2).max(1) as usize,
                        height.div_ceil(2).max(1) as usize,
                        v,
                    );
                    self.call_event_handler(&Event::VideoTextureUpdated(
                        VideoTextureUpdatedEvent {
                            video_id: live_id,
                            current_position_ms: position_ms,
                            yuv: crate::event::video_playback::VideoYuvMetadata {
                                enabled: true,
                                matrix: 1.0,
                                biplanar: false,
                                rotation_steps: 0.0,
                            },
                            metadata: VideoTextureUpdateMetadata::default(),
                        },
                    ));
                }
            }
            FromJavaMessage::VideoHardwareBufferFrame {
                video_id,
                width,
                height,
                position_ms,
                frame_sequence,
                timestamp_ns,
                hardware_buffer,
            } => {
                let live_id = LiveId(video_id);
                #[cfg(use_vulkan)]
                {
                    let update_result = if let Some(config) =
                        self.os.video_configs.get(&live_id).cloned()
                    {
                        self.os
                            .vulkan
                            .as_mut()
                            .ok_or_else(|| {
                                "Android broker H.264 hardware-buffer texture requested without Vulkan backend"
                                    .to_string()
                            })
                            .and_then(|vk| {
                                vk.update_video_external_hardware_buffer_texture(
                                    config.texture_id,
                                    hardware_buffer,
                                    width,
                                    height,
                                )
                            })
                    } else {
                        Err(format!(
                            "Android broker H.264 hardware-buffer frame for unknown video_id={video_id}"
                        ))
                    };

                    match update_result {
                        Ok((yuv, metadata)) => {
                            let frame_sequence = frame_sequence.max(1);
                            let timestamp_ns = if timestamp_ns > 0 {
                                timestamp_ns
                            } else {
                                android_video_diagnostic_time_ns()
                            };
                            let metadata = metadata
                                .with_camera_frame(frame_sequence, timestamp_ns, None)
                                .with_hardware_buffer_import(
                                    frame_sequence,
                                    android_video_diagnostic_time_ns(),
                                );
                            crate::log!(
                                "RUSTY_XR_MAKEPAD_BROKER_H264_HARDWARE_BUFFER_FRAME schema=rusty.xr.makepad-broker-h264-hardware-buffer-frame.v1 phase=texture-updated status=ok videoId={} frameSeq={} timestampNs={} width={} height={} positionMs={}",
                                video_id,
                                frame_sequence,
                                timestamp_ns,
                                width,
                                height,
                                position_ms,
                            );
                            self.call_event_handler(&Event::VideoTextureUpdated(
                                VideoTextureUpdatedEvent {
                                    video_id: live_id,
                                    current_position_ms: position_ms,
                                    yuv,
                                    metadata,
                                },
                            ));
                        }
                        Err(error) => {
                            self.call_event_handler(&Event::VideoDecodingError(
                                VideoDecodingErrorEvent {
                                    video_id: live_id,
                                    error,
                                },
                            ));
                        }
                    }
                    unsafe {
                        ndk_sys::AHardwareBuffer_release(hardware_buffer);
                    }
                }
                #[cfg(not(use_vulkan))]
                {
                    unsafe {
                        ndk_sys::AHardwareBuffer_release(hardware_buffer);
                    }
                    self.call_event_handler(&Event::VideoDecodingError(VideoDecodingErrorEvent {
                        video_id: live_id,
                        error:
                            "Android broker H.264 hardware-buffer texture requires Vulkan backend"
                                .to_string(),
                    }));
                }
            }
            FromJavaMessage::VideoHardwareBufferStereoFrame {
                left_video_id,
                left_width,
                left_height,
                left_position_ms,
                left_frame_sequence,
                left_timestamp_ns,
                left_hardware_buffer,
                right_video_id,
                right_width,
                right_height,
                right_position_ms,
                right_frame_sequence,
                right_timestamp_ns,
                right_hardware_buffer,
                pair_delta_ns,
                pair_index,
            } => {
                #[cfg(use_vulkan)]
                {
                    let left_result = self.update_android_video_external_hardware_buffer_frame(
                        left_video_id,
                        left_width,
                        left_height,
                        left_frame_sequence,
                        left_timestamp_ns,
                        left_hardware_buffer,
                    );
                    let right_result = self.update_android_video_external_hardware_buffer_frame(
                        right_video_id,
                        right_width,
                        right_height,
                        right_frame_sequence,
                        right_timestamp_ns,
                        right_hardware_buffer,
                    );
                    match (left_result, right_result) {
                        (Ok((left_yuv, left_metadata)), Ok((right_yuv, right_metadata))) => {
                            crate::log!(
                                "RUSTY_XR_MAKEPAD_BROKER_H264_STEREO_HARDWARE_BUFFER_FRAME schema=rusty.xr.makepad-broker-h264-stereo-hardware-buffer-frame.v1 phase=texture-updated status=ok pairIndex={} pairDeltaNs={} leftVideoId={} rightVideoId={} leftFrameSeq={} rightFrameSeq={} leftTimestampNs={} rightTimestampNs={} leftWidth={} leftHeight={} rightWidth={} rightHeight={}",
                                pair_index,
                                pair_delta_ns,
                                left_video_id,
                                right_video_id,
                                left_frame_sequence.max(1),
                                right_frame_sequence.max(1),
                                left_timestamp_ns,
                                right_timestamp_ns,
                                left_width,
                                left_height,
                                right_width,
                                right_height,
                            );
                            self.call_event_handler(&Event::VideoTextureUpdated(
                                VideoTextureUpdatedEvent {
                                    video_id: LiveId(left_video_id),
                                    current_position_ms: left_position_ms,
                                    yuv: left_yuv,
                                    metadata: left_metadata,
                                },
                            ));
                            self.call_event_handler(&Event::VideoTextureUpdated(
                                VideoTextureUpdatedEvent {
                                    video_id: LiveId(right_video_id),
                                    current_position_ms: right_position_ms,
                                    yuv: right_yuv,
                                    metadata: right_metadata,
                                },
                            ));
                        }
                        (Err(left_error), Err(right_error)) => {
                            self.call_event_handler(&Event::VideoDecodingError(
                                VideoDecodingErrorEvent {
                                    video_id: LiveId(left_video_id),
                                    error: left_error,
                                },
                            ));
                            self.call_event_handler(&Event::VideoDecodingError(
                                VideoDecodingErrorEvent {
                                    video_id: LiveId(right_video_id),
                                    error: right_error,
                                },
                            ));
                        }
                        (Err(error), Ok(_)) => {
                            self.call_event_handler(&Event::VideoDecodingError(
                                VideoDecodingErrorEvent {
                                    video_id: LiveId(left_video_id),
                                    error,
                                },
                            ));
                        }
                        (Ok(_), Err(error)) => {
                            self.call_event_handler(&Event::VideoDecodingError(
                                VideoDecodingErrorEvent {
                                    video_id: LiveId(right_video_id),
                                    error,
                                },
                            ));
                        }
                    }
                    unsafe {
                        ndk_sys::AHardwareBuffer_release(left_hardware_buffer);
                        ndk_sys::AHardwareBuffer_release(right_hardware_buffer);
                    }
                }
                #[cfg(not(use_vulkan))]
                {
                    unsafe {
                        ndk_sys::AHardwareBuffer_release(left_hardware_buffer);
                        ndk_sys::AHardwareBuffer_release(right_hardware_buffer);
                    }
                    self.call_event_handler(&Event::VideoDecodingError(VideoDecodingErrorEvent {
                        video_id: LiveId(left_video_id),
                        error:
                            "Android broker H.264 stereo hardware-buffer texture requires Vulkan backend"
                                .to_string(),
                    }));
                    self.call_event_handler(&Event::VideoDecodingError(VideoDecodingErrorEvent {
                        video_id: LiveId(right_video_id),
                        error:
                            "Android broker H.264 stereo hardware-buffer texture requires Vulkan backend"
                                .to_string(),
                    }));
                }
            }
            FromJavaMessage::VideoHardwareBufferStereoFrameReady { pair_index } => {
                if let Some(frame) = android_jni::take_latest_video_hardware_buffer_stereo_frame() {
                    self.handle_message(frame);
                } else if pair_index < 8 || pair_index % 120 == 0 {
                    crate::log!(
                        "RUSTY_XR_MAKEPAD_BROKER_H264_STEREO_HARDWARE_BUFFER_LATEST_SLOT schema=rusty.xr.makepad-broker-h264-stereo-hardware-buffer-latest-slot.v1 phase=take status=empty pairIndex={} policy=latest-native-slot",
                        pair_index
                    );
                }
            }
            FromJavaMessage::VideoPlaybackCompleted { video_id } => {
                let e = Event::VideoPlaybackCompleted(VideoPlaybackCompletedEvent {
                    video_id: LiveId(video_id),
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::VideoPlayerReleased { video_id } => {
                let live_id = LiveId(video_id);
                if let Some(decoder_ref) = self.os.video_surfaces.remove(&live_id) {
                    unsafe {
                        let env = attach_jni_env();
                        android_jni::to_java_cleanup_video_decoder_ref(env, decoder_ref);
                    }
                }
                if let Some(mut asp) = self.os.software_video_players.remove(&live_id) {
                    asp.player.cleanup();
                }
                self.os.video_configs.remove(&live_id);

                let e =
                    Event::VideoPlaybackResourcesReleased(VideoPlaybackResourcesReleasedEvent {
                        video_id: live_id,
                    });
                self.call_event_handler(&e);
            }
            FromJavaMessage::VideoDecodingError { video_id, error } => {
                let live_id = LiveId(video_id);
                let force_native = force_native_video();
                if !force_native && !self.os.software_video_players.contains_key(&live_id) {
                    if let Some(config) = self.os.video_configs.get(&live_id).cloned() {
                        if !config.source.supports_software_fallback() {
                            let e = Event::VideoDecodingError(VideoDecodingErrorEvent {
                                video_id: live_id,
                                error,
                            });
                            self.call_event_handler(&e);
                            return;
                        }
                        crate::log!(
                            "VIDEO: Android native decode failed for {}, falling back to software video: {}",
                            live_id.0,
                            error
                        );
                        let asp = AndroidSoftwarePlayer {
                            player: PlaybackSessionHandle::new(
                                live_id,
                                config.texture_id,
                                config.source,
                                config.autoplay,
                                config.should_loop,
                            ),
                            tex_y_id: config.tex_y_id,
                            tex_u_id: config.tex_u_id,
                            tex_v_id: config.tex_v_id,
                            yuv_matrix: 0.0,
                        };
                        self.os.software_video_players.insert(live_id, asp);
                        self.redraw_all();
                        return;
                    }
                }

                let e = Event::VideoDecodingError(VideoDecodingErrorEvent {
                    video_id: live_id,
                    error,
                });
                self.call_event_handler(&e);
            }
            FromJavaMessage::CameraPreviewSurfaceReady {
                video_id,
                window,
                width: _,
                height: _,
            } => {
                let live_id = LiveId(video_id);
                if let Some(player) = self.os.camera_players.get_mut(&live_id) {
                    player.set_preview_window(Some(window));
                } else {
                    if let Some(old) = self
                        .os
                        .pending_camera_preview_windows
                        .insert(live_id, window)
                    {
                        unsafe {
                            ndk_sys::ANativeWindow_release(old);
                        }
                    }
                }
            }
            FromJavaMessage::CameraPreviewSurfaceDestroyed { video_id } => {
                let live_id = LiveId(video_id);
                if let Some(player) = self.os.camera_players.get_mut(&live_id) {
                    player.set_preview_window(None);
                }
                if let Some(window) = self.os.pending_camera_preview_windows.remove(&live_id) {
                    unsafe {
                        ndk_sys::ANativeWindow_release(window);
                    }
                }
            }
            _ => unreachable!("non-video Android Java message routed to video handler"),
        }
    }

    fn handle_android_lifecycle_message(&mut self, msg: FromJavaMessage) {
        match msg {
            FromJavaMessage::SwitchedActivity(activity_handle, activity_thread_id) => {
                self.os.activity_thread_id = Some(activity_thread_id);
                if self.os.in_xr_mode {
                    if let Err(e) = self.os.openxr.create_instance(activity_handle) {
                        crate::error!("OpenXR init failed: {}", e);
                    }
                }
            }
            FromJavaMessage::RenderLoop => {
                // This should not happen here, as it's handled in the main loop
            }
            FromJavaMessage::Pause => {
                self.call_event_handler(&Event::Pause);
            }
            FromJavaMessage::Resume => {
                if self.os.fullscreen {
                    unsafe {
                        let env = attach_jni_env();
                        android_jni::to_java_set_full_screen(env, true);
                    }
                }
                // Java may keep a cached snapshot overlay visible across any
                // pause/resume transition, even when Android never tears down
                // the underlying SurfaceView. Always hide that overlay on the
                // first successful present after resume.
                self.os.hide_surface_cover_after_first_present = true;
                self.os.refresh_surface_snapshot_after_first_present = true;
                self.redraw_all();
                self.reinitialise_media();
                self.call_event_handler(&Event::Resume);
            }

            FromJavaMessage::Start => {
                self.call_event_handler(&Event::Foreground);
            }
            FromJavaMessage::Stop => {
                self.call_event_handler(&Event::Background);
            }
            FromJavaMessage::Destroy => {
                android_jni::clear_latest_video_hardware_buffer_stereo_frame();
                if !self.os.ignore_destroy {
                    self.call_event_handler(&Event::Shutdown);
                    self.os.quit = true;
                }

                self.os.ignore_destroy = false;
            }
            FromJavaMessage::WindowFocusChanged { has_focus } => {
                let window_id = CxWindowPool::id_zero();
                if has_focus {
                    self.call_event_handler(&Event::WindowGotFocus(window_id));
                } else {
                    self.call_event_handler(&Event::WindowLostFocus(window_id));
                }
            }
            FromJavaMessage::Init(_) => {}
            _ => unreachable!("non-lifecycle Android Java message routed to lifecycle handler"),
        }
    }
}
