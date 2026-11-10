/*
 * SmartKey C/C++ FFI Header (v0.1.2)
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef SMARTKEY_H
#define SMARTKEY_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Threading & error contract:
 *   - `SmartKeyEngineHandle` and `SmartKeySessionHandle` are single-threaded
 *     handles designed to be driven on the host input-method event loop. Do not
 *     share handles concurrently across threads without external synchronization.
 *   - Passing `NULL` for `rules_path_utf8` explicitly constructs an in-memory
 *     engine; passing a non-NULL empty or invalid UTF-8 path fails and returns
 *     `NULL`.
 */
typedef struct SmartKeyEngineHandle SmartKeyEngineHandle;
typedef struct SmartKeySessionHandle SmartKeySessionHandle;

/* Preset IDs for `smartkey_engine_new` */
#define SMARTKEY_PRESET_BALANCED         0
#define SMARTKEY_PRESET_CHAT_CASUAL      1
#define SMARTKEY_PRESET_DEVELOPER_STRICT 2

/* Application profiles for `smartkey_session_set_profile` */
#define SMARTKEY_PROFILE_BYPASS  0
#define SMARTKEY_PROFILE_BROWSER 1
#define SMARTKEY_PROFILE_CHAT    2
#define SMARTKEY_PROFILE_PROSE   3
#define SMARTKEY_PROFILE_NONE    255

/* Key kinds for `smartkey_session_process_key` */
#define SMARTKEY_KEY_CHAR           0
#define SMARTKEY_KEY_BACKSPACE      1
#define SMARTKEY_KEY_CTRL_BACKSPACE 2
#define SMARTKEY_KEY_ENTER          3
#define SMARTKEY_KEY_TAB            4
#define SMARTKEY_KEY_NAV_OR_CLICK   5
#define SMARTKEY_KEY_FOCUS_LOST     6

/* Action kinds returned in `SmartKeyAction::kind` */
#define SMARTKEY_ACTION_PASS_THROUGH 0
#define SMARTKEY_ACTION_REPLACE      1

/*
 * Result of processing a single keystroke.
 * When `kind == SMARTKEY_ACTION_REPLACE`:
 *   - Consume/filter the incoming key event.
 *   - Delete `delete_chars` Unicode scalar characters before the cursor.
 *   - Commit `insert_utf8` (`insert_byte_len` UTF-8 bytes, NUL-terminated).
 * `insert_utf8` is owned by `SmartKeySessionHandle` and stays valid until the
 * next mutating call on that session or `smartkey_session_free`.
 */
typedef struct SmartKeyAction {
    uint8_t kind;
    uint32_t delete_chars;
    const char* insert_utf8;
    uint32_t insert_byte_len;
} SmartKeyAction;

/* Engine lifecycle */
SmartKeyEngineHandle* smartkey_engine_new(
    const char* rules_path_utf8,
    const char* state_path_utf8,
    uint8_t preset
);

SmartKeyEngineHandle* smartkey_engine_new_with_lexicon_bytes(
    const uint8_t* skdict_bytes,
    size_t skdict_len,
    const char* rules_path_utf8,
    const char* state_path_utf8,
    uint8_t preset
);

void smartkey_engine_free(SmartKeyEngineHandle* engine);
bool smartkey_engine_reload_dicts(SmartKeyEngineHandle* engine);

/* Session lifecycle & context */
SmartKeySessionHandle* smartkey_session_new(void);
void smartkey_session_free(SmartKeySessionHandle* session);
void smartkey_session_reset(SmartKeySessionHandle* session, SmartKeyEngineHandle* engine);
void smartkey_session_set_profile(SmartKeySessionHandle* session, uint8_t profile);
void smartkey_session_set_lang(SmartKeySessionHandle* session, const char* lang_utf8);
void smartkey_session_set_app(SmartKeySessionHandle* session, const char* app_utf8);

/* Durable diagnostics event log (`~/.local/state/smartkey/logs/`) */
bool smartkey_engine_set_log_dir(SmartKeyEngineHandle* engine, const char* log_dir_utf8);
void smartkey_engine_log_app_event(
    SmartKeyEngineHandle* engine,
    const char* app_utf8,
    uint8_t profile,
    const char* reason_utf8
);

/*
 * Returns a `SMARTKEY_PROFILE_*` override from `[app_profiles]` in `rules.toml`
 * (including `unknown_app_policy` when `app_utf8` is `""` or `NULL`), or
 * `SMARTKEY_PROFILE_NONE` (`255`) when no user override matches.
 */
uint8_t smartkey_engine_classify_app_override(
    const SmartKeyEngineHandle* engine,
    const char* app_utf8
);

/* Desktop keystroke stream (`fcitx5-lotus` / X11 / Wayland) */
bool smartkey_session_process_key(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    uint8_t key_kind,
    uint32_t codepoint,
    uint64_t now_ms,
    SmartKeyAction* out_action
);

void smartkey_session_reconcile_surrounding(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    const char* surrounding_utf8,
    uint32_t cursor_chars,
    uint64_t now_ms
);

/* Bilingual `fcitx5-lotus` queries, commit reconciliation & `telex_passthrough` learning */
void smartkey_session_set_lotus_composing(
    SmartKeySessionHandle* session,
    bool composing
);

bool smartkey_session_on_lotus_commit(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    uint32_t boundary_codepoint,
    const char* committed_utf8,
    uint64_t now_ms,
    SmartKeyAction* out_action
);
bool smartkey_engine_is_vi_lotus_enabled(
    const SmartKeyEngineHandle* engine
);

bool smartkey_engine_is_english_word(
    const SmartKeyEngineHandle* engine,
    const char* word_utf8,
    uint8_t* out_freq
);

bool smartkey_engine_is_telex_passthrough(
    const SmartKeyEngineHandle* engine,
    const char* raw_ascii_utf8
);

bool smartkey_session_record_telex_passthrough_repair(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    const char* raw_ascii_utf8
);

/* Portable word-level learner hooks (Android HeliBoard JNI) */
bool smartkey_engine_is_pair_blocked(
    const SmartKeyEngineHandle* engine,
    const char* typed_utf8,
    const char* candidate_utf8
);

bool smartkey_engine_is_personal_word(
    const SmartKeyEngineHandle* engine,
    const char* word_utf8
);

void smartkey_mobile_on_auto_commit(
    SmartKeySessionHandle* session,
    const char* raw_typed_utf8,
    const char* applied_utf8
);

void smartkey_mobile_on_undo_keep(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    const char* raw_typed_utf8,
    const char* rejected_utf8
);

void smartkey_mobile_on_manual_fix(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    const char* orig_utf8,
    const char* fixed_utf8
);

void smartkey_mobile_on_untouched_commit(
    SmartKeySessionHandle* session,
    SmartKeyEngineHandle* engine,
    const char* word_utf8
);

#ifdef __cplusplus
}
#endif

#endif /* SMARTKEY_H */
