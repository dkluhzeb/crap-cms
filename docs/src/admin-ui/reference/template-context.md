<!--
  AUTO-GENERATED — do not edit by hand.
  Source of truth: typed page-context structs in `src/admin/context/page/`.
  Regenerate with: `cargo xtask gen-template-doc`
-->

# Admin template context reference

Every admin page renders a typed Rust struct serialized to JSON, runs it through the optional `before_render` Lua hook, and hands it to Handlebars. This file lists every page, its `page.type` discriminant, the template it renders, and the fields the template can rely on.

Field types use Rust-style notation: `string`, `integer`, `boolean`, `Vec<T>`, `Option<T>`. Composite leaves like `CrapMeta`, `NavData`, `FieldContext` link into the [shared definitions](#shared-definitions) section at the bottom.

## Login page

- **`page.type`**: `auth_login`
- **Template**: `templates/auth/login.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec&lt;string&gt;)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`error`** (Option&lt;string&gt;) _(optional)_ — Error key (e.g., `"error_invalid_credentials"`) — present after a failed login post.
- **`email`** (Option&lt;string&gt;) _(optional)_ — Pre-fills the email field after a failed login.
- **`collections`** (Vec&lt;[AuthCollection](#authcollection)&gt;)
- **`show_collection_picker`** (boolean)
- **`disable_local`** (boolean)
- **`show_forgot_password`** (boolean)
- **`success`** (Option&lt;string&gt;) _(optional)_ — Whitelisted success-message key shown after redirect from logout / email verification / password reset. Always emitted (as `null` when absent) to preserve the original `Option`-as-null contract.

## MFA challenge page

- **`page.type`**: `auth_mfa`
- **Template**: `templates/auth/mfa.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec&lt;string&gt;)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`error`** (Option&lt;string&gt;) _(optional)_
- **`totp`** (boolean) — TOTP mode: the entry-form wording switches to authenticator-app.
- **`totp_provisioning_uri`** (Option&lt;string&gt;) _(optional)_ — TOTP enrollment (shown only while unconfirmed): the `otpauth://` link an authenticator app consumes.
- **`totp_secret`** (Option&lt;string&gt;) _(optional)_ — The base32 secret for manual entry (unconfirmed enrollment only).

## Forgot password page

- **`page.type`**: `auth_forgot`
- **Template**: `templates/auth/forgot_password.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec&lt;string&gt;)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`success`** (boolean)
- **`collections`** (Vec&lt;[AuthCollection](#authcollection)&gt;)
- **`show_collection_picker`** (boolean)

## Reset password page

- **`page.type`**: `auth_reset`
- **Template**: `templates/auth/reset_password.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec&lt;string&gt;)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`token`** (Option&lt;string&gt;) _(optional)_ — Token from the URL — present only when valid. Absent when the link is bad / expired (in which case `error` is set instead).
- **`error`** (Option&lt;string&gt;) _(optional)_

## Error pages (400 / 403 / 404 / 500)

- **`page.type`**: `error_400 | error_403 | error_404 | error_500`
- **Template**: `templates/errors/{400,403,404,500}.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`message`** (string) — User-facing error message body.

## Dashboard

- **`page.type`**: `dashboard`
- **Template**: `templates/dashboard/index.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection_cards`** (Vec&lt;[CollectionCard](#collectioncard)&gt;)
- **`global_cards`** (Vec&lt;[GlobalCard](#globalcard)&gt;)

## Collection list

- **`page.type`**: `collection_list`
- **Template**: `templates/collections/list.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collections`** (Vec&lt;[CollectionEntry](#collectionentry)&gt;)

## Collection items list

- **`page.type`**: `collection_items`
- **Template**: `templates/collections/items.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`perms`** ([CollectionPermissions](#collectionpermissions))
- **`docs`** (Vec&lt;any&gt;)
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`has_drafts`** (boolean)
- **`has_soft_delete`** (boolean)
- **`is_trash`** (boolean)
- **`search`** (Option&lt;string&gt;) _(optional)_
- **`sort`** (Option&lt;string&gt;) _(optional)_
- **`table_columns`** (Vec&lt;any&gt;)
- **`column_options`** (Vec&lt;any&gt;)
- **`filter_fields`** (Vec&lt;any&gt;)
- **`active_filters`** (Vec&lt;any&gt;)
- **`active_filter_count`** (integer)
- **`title_sort_url`** (Option&lt;string&gt;) _(optional)_
- **`title_sorted_asc`** (boolean)
- **`title_sorted_desc`** (boolean)

## Collection edit form

- **`page.type`**: `collection_edit`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`perms`** ([CollectionPermissions](#collectionpermissions))
- **`document`** ([DocumentRef](#documentref))
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`sidebar_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** (Vec&lt;any&gt;)
- **`has_more_versions`** (boolean)
- **`restore_url_prefix`** (string)
- **`versions_url`** (string)
- **`document_title`** (string)
- **`ref_count`** (integer)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec&lt;[LocaleTemplateOption](#localetemplateoption)&gt;) _(optional)_
- **`upload`** ([UploadFormContext](#uploadformcontext) \| null) _(optional)_ — Upload preview block — present only on upload collections.

## Collection create form

- **`page.type`**: `collection_create`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`perms`** ([CollectionPermissions](#collectionpermissions))
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`sidebar_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec&lt;[LocaleTemplateOption](#localetemplateoption)&gt;) _(optional)_
- **`upload`** ([UploadFormContext](#uploadformcontext) \| null) _(optional)_

## Collection form-error re-render

- **`page.type`**: `collection_edit | collection_create`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`perms`** ([CollectionPermissions](#collectionpermissions))
- **`document`** ([DocumentRef](#documentref) \| null) _(optional)_ — Document stub (with `id` only) on edit error; absent on create error.
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`sidebar_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`upload_hidden_fields`** (Option&lt;Vec&lt;any&gt;&gt;) _(optional)_ — Hidden upload fields preserved from the submitted form (edit-mode upload errors only, so the user keeps their pending file metadata).

## Collection delete confirmation

- **`page.type`**: `collection_delete`
- **Template**: `templates/collections/delete.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document_id`** (string)
- **`title_value`** (Option&lt;string&gt;) _(optional)_ — Document title for display. `None` (serialized as `null`) when the collection has no title field or the read fell through.
- **`ref_count`** (integer)

## Collection versions list

- **`page.type`**: `collection_versions`
- **Template**: `templates/collections/versions.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref))
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`doc_title`** (string)
- **`versions`** (Vec&lt;any&gt;)
- **`restore_url_prefix`** (string)

## Collection restore confirmation

- **`page.type`**: `collection_versions`
- **Template**: `templates/collections/restore.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref))
- **`version_number`** (any) — Version number being restored (from the version row's `version` column).
- **`missing_relations`** (Vec&lt;any&gt;) — IDs of relationship references whose targets no longer exist.
- **`restore_url`** (string)
- **`back_url`** (string)

## Global edit form

- **`page.type`**: `global_edit`
- **Template**: `templates/globals/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`perms`** ([GlobalPermissions](#globalpermissions))
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`sidebar_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** (Vec&lt;any&gt;)
- **`has_more_versions`** (boolean)
- **`restore_url_prefix`** (string)
- **`versions_url`** (string)
- **`doc_status`** (string)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec&lt;[LocaleTemplateOption](#localetemplateoption)&gt;) _(optional)_

## Global form-error re-render

- **`page.type`**: `global_edit`
- **Template**: `templates/globals/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`perms`** ([GlobalPermissions](#globalpermissions))
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)
- **`sidebar_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;)

## Global versions list

- **`page.type`**: `global_versions`
- **Template**: `templates/globals/versions.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`versions`** (Vec&lt;any&gt;)
- **`restore_url_prefix`** (string)

## Global restore confirmation

- **`page.type`**: `global_versions`
- **Template**: `templates/globals/restore.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`version_number`** (any)
- **`missing_relations`** (Vec&lt;any&gt;)
- **`restore_url`** (string)
- **`back_url`** (string)

## Custom admin page

- **`page.type`**: `custom_page`
- **Template**: `templates/pages/{slug}.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_
- **`slug`** (string) — Slug from the URL — also the filename stem of the rendered template (e.g. `status` → `templates/pages/status.hbs`).


---

## Shared definitions

Every page above flattens [BasePageContext](#basepagecontext) (or [AuthBasePageContext](#authbasepagecontext) for auth-flow pages) into its top-level fields. The base structs and their leaves are defined here once.

### BasePageContext

Common fields present on every authenticated admin page.

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec&lt;string&gt;) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option&lt;boolean&gt;) _(optional)_
- **`editor_locale`** (Option&lt;string&gt;) _(optional)_
- **`editor_locales`** (Option&lt;Vec&lt;[EditorLocaleOption](#editorlocaleoption)&gt;&gt;) _(optional)_

### AuthBasePageContext

Minimal base for unauthenticated pages (login / forgot / reset / MFA).
Omits `nav` and `user`.

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec&lt;string&gt;)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))

### PageMeta

The `page` object every admin template receives. Carries the page-type
discriminant, the page title (already-translated label or translation key),
optional title interpolation parameter, and breadcrumb trail.

- **`type`** (string) — Page-type discriminant. Serialized as a `snake_case` string literal so templates can branch with `{{#if (eq page.type "collection_edit")}}`.
- **`title`** (string) — Page title or translation key.
- **`title_name`** (Option&lt;string&gt;) _(optional)_ — Optional interpolation param for `{{t page.title name=page.title_name}}`.
- **`breadcrumbs`** (Vec&lt;[Breadcrumb](#breadcrumb)&gt;) — Breadcrumb trail rendered by `partials/breadcrumb.hbs`.

### CrapMeta

Metadata about the running crap-cms process and current request.

- **`version`** (string) — Crate version (Cargo.toml `version`).
- **`build_hash`** (string) — Build hash (set by build script from git).
- **`dev_mode`** (boolean) — Whether admin dev-mode is enabled (per-request template reload, etc.).
- **`auth_enabled`** (boolean) — Whether the system has any auth-enabled collections.
- **`csp_nonce`** (string) — Per-request CSP nonce (empty string outside request scope).
- **`site_name`** (string) — Branding string shown in the admin header wordmark and `<title>` tags. Defaults to `"Crap CMS"`; configurable via `[admin] site_name = "..."` in `crap.toml`.

### NavData

Top-level nav data exposed at `{{nav.*}}`.

- **`collections`** (Vec&lt;[NavCollection](#navcollection)&gt;)
- **`globals`** (Vec&lt;[NavGlobal](#navglobal)&gt;)
- **`custom_pages`** (Vec&lt;[CustomPage](#custompage)&gt;) — Filesystem-routed custom admin pages registered via `crap.pages.register`. Only entries with a `label` set appear here.

### NavCollection

One sidebar entry for a collection.

- **`slug`** (string)
- **`display_name`** (string)
- **`is_auth`** (boolean)
- **`is_upload`** (boolean)

### NavGlobal

One sidebar entry for a global.

- **`slug`** (string)
- **`display_name`** (string)

### UserContext

Identifying data about the currently authenticated user.

- **`email`** (string)
- **`id`** (string)
- **`collection`** (string)

### EditorLocaleOption

One row in the editor-locale picker dropdown.

- **`value`** (string)
- **`label`** (string)
- **`selected`** (boolean)

### LocaleTemplateOption

Per-locale option in the template-data picker.

- **`value`** (string)
- **`label`** (string)
- **`selected`** (boolean)

### Breadcrumb

A breadcrumb entry with a label and optional URL.

- **`label`** (string) — The text label to display for the breadcrumb.
- **`url`** (Option&lt;string&gt;) _(optional)_ — The optional URL to link to. If None, the breadcrumb is the current page.
- **`label_name`** (Option&lt;string&gt;) _(optional)_ — Optional interpolation param for `{{t label name=label_name}}`.

### CollectionContext

Top-level collection metadata exposed to templates.

Capability bits (`is_auth`, `is_upload`, `has_versions`, `has_drafts`)
are intentionally NOT separate fields — templates derive them from the
presence of the corresponding `Option<*Meta>` sub-struct (and the
`drafts` flag inside `versions`). Single source of truth: if `auth`
is `None`, the collection isn't auth-enabled; if `versions.drafts` is
false, drafts aren't on.

- **`slug`** (string)
- **`display_name`** (string)
- **`singular_name`** (string)
- **`title_field`** (Option&lt;string&gt;) _(optional)_
- **`timestamps`** (boolean)
- **`soft_delete`** (boolean)
- **`admin`** ([AdminMeta](#adminmeta))
- **`upload`** ([UploadMeta](#uploadmeta) \| null) _(optional)_
- **`versions`** ([VersionsMeta](#versionsmeta) \| null) _(optional)_
- **`auth`** ([AuthMeta](#authmeta) \| null) _(optional)_
- **`fields_meta`** (Vec&lt;[FieldMeta](#fieldmeta)&gt;)

### CollectionPermissions

Per-user permissions for a collection page.

Field semantics:
- `read` — can the user view the collection at all. This is the **union**
  of the document-list views the read path serves (published ∪ draft ∪
  trash, each resolved with its fallback), matching the service's
  union-and-downgrade read model — a user allowed only drafts (or only
  trash) can still view the collection, so `read` is `true` for them.
- `create` — can the user create new items.
- `update` — can the user update existing items (drives the Save /
  Publish / Save Draft / Unpublish row in the edit sidebar).
- `delete` — can the user *hard*-delete items (drives "Empty Trash"
  and per-row "Delete permanently" buttons).
- `trash` — can the user soft-delete items. Only meaningful for
  collections with `soft_delete = true`. When `def.access.trash` is
  unset, falls back to `update` (via `resolve_trash()`, matching the
  soft-delete enforcement path).

- **`read`** (boolean)
- **`create`** (boolean)
- **`update`** (boolean)
- **`delete`** (boolean)
- **`trash`** (boolean)
- **`draft`** (boolean) — Whether the user may view draft (unpublished) content — gated on `resolve_draft()` (`access.draft`, or `access.update` as the fallback), mirroring what the read paths enforce. A pure UI hint (e.g. a Drafts tab): the read paths request every view unconditionally and the service downgrades per access, so this never gates the request itself.
- **`versions`** (boolean) — Whether the user may view version history — gated on `resolve_versions()` (`access.versions`, or `access.update` as the fallback), matching the service gate. Drives whether the version-history sidebar panel is shown. Only meaningful when `versions` is enabled. A UI hint only; the service enforces the real gate.

### AdminMeta

Admin-presentation metadata pulled from `def.admin`.

- **`use_as_title`** (Option&lt;string&gt;) _(optional)_
- **`default_sort`** (Option&lt;string&gt;) _(optional)_
- **`hidden`** (boolean)
- **`list_searchable_fields`** (Vec&lt;string&gt;)

### AuthMeta

Auth-collection metadata. Only present when `def.auth` is set.

- **`enabled`** (boolean)
- **`disable_local`** (boolean)
- **`verify_email`** (boolean)

### UploadMeta

Upload-collection metadata. Only present when `def.upload` is set.

- **`enabled`** (boolean)
- **`mime_types`** (Vec&lt;string&gt;)
- **`max_file_size`** (Option&lt;integer&gt;) _(optional)_
- **`admin_thumbnail`** (Option&lt;string&gt;) _(optional)_

### VersionsMeta

Versioning metadata. Only present when `def.versions` is set.

- **`drafts`** (boolean)
- **`max_versions`** (integer)

### FieldMeta

Metadata about a single field as it appears to templates.

- **`name`** (string)
- **`field_type`** (string)
- **`required`** (boolean)
- **`unique`** (boolean)
- **`localized`** (boolean)
- **`admin`** ([FieldAdminMeta](#fieldadminmeta))

### FieldAdminMeta

Admin-presentation metadata for a field (label, description, layout hints).

- **`label`** (Option&lt;string&gt;) _(optional)_
- **`hidden`** (boolean)
- **`readonly`** (boolean)
- **`width`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_

### GlobalContext

Top-level global metadata exposed to templates.

- **`slug`** (string)
- **`display_name`** (string)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** ([VersionsMeta](#versionsmeta) \| null) _(optional)_
- **`fields_meta`** (Vec&lt;[FieldMeta](#fieldmeta)&gt;)

### GlobalPermissions

Per-user permissions for a global page. Globals only have `read` and
`update` access — no create/delete (the row always exists).

- **`read`** (boolean)
- **`update`** (boolean)
- **`draft`** (boolean) — Whether the user may view the global's draft (unpublished) content — gated on `resolve_draft()` (`access.draft`, or `access.update`). A UI hint only — the read path requests drafts unconditionally and downgrades.
- **`versions`** (boolean) — Whether the user may view the global's version history — gated on `resolve_versions()` (`access.versions`, or `access.update`). Drives whether the version-history panel is shown. A UI hint only.

### DocumentRef

A document reference exposed at `{{document.*}}`. The `data` map carries the
document's field values (untyped — typing field values is part of 1.C.2).

- **`id`** (string)
- **`created_at`** (Option&lt;string&gt;) _(optional)_
- **`updated_at`** (Option&lt;string&gt;) _(optional)_
- **`status`** (Option&lt;string&gt;) _(optional)_
- **`data`** ([DocumentFields](#documentfields) \| null) _(optional)_

### DocumentFields

A document's user-defined field values. See module docs.

_(No fields.)_

### ConditionExpr

A display-condition expression. Accepts either a single row or an
array of rows AND'd together.

Untagged union — one of: Vec&lt;[ConditionRow](#conditionrow)&gt; \| [ConditionRow](#conditionrow).

### ConditionRow

One row of a `ConditionExpr`: a field reference plus an operator.

- **`field`** (string)

### TimezoneOption

One row in a Date field's timezone picker.

- **`value`** (string) _(optional)_
- **`label`** (string) _(optional)_

### CustomPage

Sidebar metadata declared from Lua via `crap.pages.register`.

- **`slug`** (string) — Slug — the URL segment and the filename stem.
- **`section`** (Option&lt;string&gt;) _(optional)_ — Sidebar section heading. `None` → page is registered but not grouped (renders ungrouped at the bottom).
- **`label`** (Option&lt;string&gt;) _(optional)_ — Sidebar label. `None` → page is registered but not shown in nav.
- **`icon`** (Option&lt;string&gt;) _(optional)_ — Optional Material Symbols icon name.
- **`access`** (Option&lt;string&gt;) _(optional)_ — Optional Lua function-ref name for access control. When set, the named function is called with the page context before the route handler renders; returning `false` produces a 403, and the page is hidden from the sidebar nav for users who can't read it. Mirrors `access.read` on collections / globals — register the function once via `crap.access.register("name", fn)`, then refer to it by name here. A bare ref string or a `{ ref, options }` table whose options reach the gate as `ctx.options`.

### PaginationContext

Pagination metadata for list views.

- **`per_page`** (integer)
- **`total`** (integer)
- **`has_prev`** (boolean)
- **`has_next`** (boolean)
- **`prev_url`** (string)
- **`next_url`** (string)
- **`page`** (Option&lt;integer&gt;) _(optional)_ — Page-mode only — current page number (1-indexed).
- **`total_pages`** (Option&lt;integer&gt;) _(optional)_ — Page-mode only — total page count.

### FieldContext

Typed admin form field context — one variant per
`FieldType`.

Internally tagged on `field_type` (lowercase variant name) so the
serialized JSON has `{"field_type": "text", ...flat fields...}`. This is
the single source of truth for the discriminator — `BaseFieldData`
does NOT carry a `field_type` field.

Internally tagged enum — `field_type` selects the variant: `text`, `email`, `password`, `json`, `textarea`, `number`, `code`, `richtext`, `date`, `checkbox`, `select`, `radio`, `relationship`, `upload`, `join`, `group`, `row`, `collapsible`, `tabs`, `array`, `blocks`. Every variant flattens its variant struct (defined below) into the top level, so templates read `field_type` plus the struct's fields directly.

### BaseFieldData

Common keys present on every field context. Variants flatten this into
themselves via `#[serde(flatten)]` so the rendered JSON has no nesting.

`placeholder` and `description` are NOT skipped when None — the existing
builder always emits them as `null` so templates that distinguish
`null` from `undefined` keep working. (Most templates branch with
`{{#if placeholder}}` which treats both identically; the explicit-null
form is preserved for parity.)

**No `field_type` field.** The discriminator is provided by the
internally-tagged `FieldContext` enum.

`Default` + `#[serde(default)]` are derived so the existing Value-based
enrichment code (which constructs ad-hoc sub-field contexts without
every base field) can roundtrip through `Deserialize` without panicking
on missing keys. The trade-off: typed handlers must explicitly populate
fields they care about; missing fields get sensible defaults silently.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.

### ValidationAttrs

Validation attributes shared by all field types — present only when the
field definition declares them.

- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_

### ConditionData

Display-condition state injected by
`apply_display_conditions`.

JSON keys keep the `condition_*` prefix for the JS evaluator at
`static/components/conditions.js`; Rust field names drop it because the
struct itself is named `ConditionData`.

- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.

### TextField

Text-like field. Variants: `Text`, `Email`, `Password`, `Json`.

Only `Text` (and `Number`) supports `has_many` — the others always
have `has_many: None` and `tags: None`.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`has_many`** (Option&lt;boolean&gt;) _(optional)_ — Set to `Some(true)` when the field is configured as a tag list.
- **`tags`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_ — Parsed tag list (when `has_many` is true; absent otherwise).

### TextareaField

Multi-line textarea. Always emits `rows` and `resizable`.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`rows`** (integer) _(optional)_ — Number of visible text rows.
- **`resizable`** (boolean) _(optional)_ — Whether the textarea allows user-resizing in the admin UI.

### NumberField

Numeric input. `step` is always emitted (default `"any"`).

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`step`** (string) _(optional)_ — HTML `step` attribute. `"any"` allows arbitrary precision.
- **`has_many`** (Option&lt;boolean&gt;) _(optional)_
- **`tags`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_

### CodeField

Source-code editor field (`CodeMirror`). Always emits `language`. Emits
`languages` only when the operator configured an allow-list (which makes
the editor render an in-form picker).

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`language`** (string) _(optional)_ — Editor language (e.g. `"json"`, `"javascript"`).
- **`languages`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_ — Optional allow-list — when present, the admin UI renders a language picker and a hidden `_lang` companion input.

### RichtextField

Rich-text editor field (`ProseMirror`). The `_node_names` key is prefixed
with `_` per the existing on-the-wire shape consumed by the
`<crap-richtext>` element.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`resizable`** (boolean) _(optional)_ — Whether the editor is user-resizable.
- **`richtext_format`** (string) _(optional)_ — Storage format. Currently `"html"` or `"json"`. Always emitted; the builder defaults to `"html"`.
- **`features`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_ — Optional list of enabled toolbar features.
- **`_node_names`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_ — Optional list of allowed `ProseMirror` node names. Emitted with a leading underscore per the existing client-side contract. Removed from the JSON by enrichment (replaced by `Self::custom_nodes`).
- **`custom_nodes`** (Option&lt;Vec&lt;[RichtextNodeDefCtx](#richtextnodedefctx)&gt;&gt;) _(optional)_ — Resolved custom node definitions — populated by enrichment from the names in `Self::node_names`.

### RichtextNodeDefCtx

One custom `ProseMirror` node definition exposed to the richtext editor.

- **`name`** (string) _(optional)_
- **`label`** (string) _(optional)_
- **`inline`** (boolean) _(optional)_
- **`attrs`** (Vec&lt;[RichtextNodeAttrCtx](#richtextnodeattrctx)&gt;) _(optional)_

### RichtextNodeAttrCtx

One attribute on a custom richtext node — describes a form field rendered
in the node-edit modal. Many fields are optional and only emitted when
configured.

- **`name`** (string) _(optional)_
- **`type`** (string) _(optional)_ — The HTML form-field type discriminator (`text`, `number`, `select`, …). Renamed because `type` is a Rust keyword.
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`default`** (any) _(optional)_
- **`options`** (Option&lt;Vec&lt;[RichtextNodeAttrOption](#richtextnodeattroption)&gt;&gt;) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`hidden`** (Option&lt;boolean&gt;) _(optional)_
- **`readonly`** (Option&lt;boolean&gt;) _(optional)_
- **`width`** (Option&lt;string&gt;) _(optional)_
- **`step`** (Option&lt;string&gt;) _(optional)_
- **`rows`** (Option&lt;integer&gt;) _(optional)_
- **`language`** (Option&lt;string&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min_date`** (Option&lt;string&gt;) _(optional)_
- **`max_date`** (Option&lt;string&gt;) _(optional)_
- **`picker_appearance`** (Option&lt;string&gt;) _(optional)_

### RichtextNodeAttrOption

One row in a richtext node attribute's `options` list (Select/Radio attrs).

- **`label`** (string) _(optional)_
- **`value`** (string) _(optional)_

### DateField

Date / datetime picker field.

Either `date_only_value` (when `picker_appearance == "dayOnly"`) or
`datetime_local_value` (when `picker_appearance == "dayAndTime"`) is set
— never both. Other appearances emit neither.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`picker_appearance`** (string) _(optional)_ — One of `"dayOnly"`, `"dayAndTime"`. Defaults to `"dayOnly"`.
- **`date_only_value`** (Option&lt;string&gt;) _(optional)_ — Set when `picker_appearance == "dayOnly"` — the `YYYY-MM-DD` slice.
- **`datetime_local_value`** (Option&lt;string&gt;) _(optional)_ — Set when `picker_appearance == "dayAndTime"` — the `YYYY-MM-DDTHH:MM` slice for the `<input type="datetime-local">`.
- **`min_date`** (Option&lt;string&gt;) _(optional)_
- **`max_date`** (Option&lt;string&gt;) _(optional)_
- **`timezone_enabled`** (Option&lt;boolean&gt;) _(optional)_
- **`default_timezone`** (Option&lt;string&gt;) _(optional)_
- **`timezone_options`** (Option&lt;Vec&lt;[TimezoneOption](#timezoneoption)&gt;&gt;) _(optional)_
- **`timezone_value`** (Option&lt;string&gt;) _(optional)_

### CheckboxField

Boolean checkbox. `checked` is always present.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`checked`** (boolean) _(optional)_

### ChoiceField

Select dropdown or radio button group. The `field_type` discriminator
on `base` distinguishes the two; the data shape is identical.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`options`** (Vec&lt;[SelectOption](#selectoption)&gt;) _(optional)_
- **`has_many`** (Option&lt;boolean&gt;) _(optional)_ — Set to `Some(true)` for multi-select; absent otherwise.

### SelectOption

One row in a Select/Radio's `options` array.

- **`label`** (string) _(optional)_
- **`value`** (string) _(optional)_
- **`selected`** (boolean) _(optional)_

### RelationshipField

Relationship to documents in another collection. The `selected_items`
field is `None` after the build phase and `Some` after enrichment.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`relationship_collection`** (Option&lt;string&gt;) _(optional)_
- **`has_many`** (Option&lt;boolean&gt;) _(optional)_
- **`polymorphic`** (Option&lt;boolean&gt;) _(optional)_ — Set to `Some(true)` for polymorphic relationships (multiple possible target collections). Templates branch on this to render a collection picker.
- **`collections`** (Option&lt;Vec&lt;string&gt;&gt;) _(optional)_ — Allowed target collections when `polymorphic` is true.
- **`picker`** (Option&lt;string&gt;) _(optional)_ — UI picker style — `"drawer"`, `"inline"`, etc.
- **`selected_items`** (Option&lt;Vec&lt;[RelationshipSelectedItem](#relationshipselecteditem)&gt;&gt;) _(optional)_ — Selected items resolved from the DB during enrichment. `collection` is `None` for non-polymorphic relationships and `Some(target_collection)` for polymorphic ones.

### RelationshipSelectedItem

One row of a `selected_items` list. For polymorphic relationships the
`collection` field is set so templates can render labels like
`{collection} / {label}`. Upload `selected_items` reuse this same struct
and populate `thumbnail_url`, `is_image`, and `filename`.

- **`id`** (string) _(optional)_
- **`label`** (string) _(optional)_
- **`collection`** (Option&lt;string&gt;) _(optional)_ — Set only for polymorphic relationships; absent for the common case.
- **`thumbnail_url`** (Option&lt;string&gt;) _(optional)_ — Upload-only — preview URL for the upload's thumbnail.
- **`is_image`** (Option&lt;boolean&gt;) _(optional)_ — Upload-only — `Some(true)` when the underlying mime starts with `image/`.
- **`filename`** (Option&lt;string&gt;) _(optional)_ — Upload-only — present when the item came from a has-one upload that also sets the form's hidden filename input.

### UploadField

Upload reference (specialised relationship to a media collection).

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`relationship_collection`** (Option&lt;string&gt;) _(optional)_
- **`has_many`** (Option&lt;boolean&gt;) _(optional)_
- **`picker`** (Option&lt;string&gt;) _(optional)_ — UI picker style — defaults to `"drawer"`. Absent when the field declares `picker = "none"`.
- **`selected_items`** (Option&lt;Vec&lt;[RelationshipSelectedItem](#relationshipselecteditem)&gt;&gt;) _(optional)_ — Resolved selected items (after enrichment).
- **`selected_filename`** (Option&lt;string&gt;) _(optional)_ — Has-one only — the resolved filename, populated by enrichment for the hidden filename input.
- **`selected_preview_url`** (Option&lt;string&gt;) _(optional)_ — Has-one only — the resolved thumbnail URL for image previews.

### JoinField

Read-only inverse-reference field. The `readonly` flag on
`BaseFieldData` is set to `true` for join fields.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`join_collection`** (Option&lt;string&gt;) _(optional)_
- **`join_on`** (Option&lt;string&gt;) _(optional)_
- **`join_items`** (Option&lt;Vec&lt;[JoinItem](#joinitem)&gt;&gt;) _(optional)_ — Reverse-lookup items resolved by enrichment for the join target.
- **`join_count`** (Option&lt;integer&gt;) _(optional)_ — Convenience count of `join_items`. Templates branch on this with `{{#if join_count}}…{{/if}}`.

### JoinItem

One row of a `JoinField::join_items` list — the inverse-reference
document's id and display label.

- **`id`** (string) _(optional)_
- **`label`** (string) _(optional)_

### GroupField

Inline group of sub-fields with `__`-prefixed column names. Also used
for the `Collapsible` variant — they share the exact JSON shape.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_
- **`collapsed`** (boolean) _(optional)_

### RowField

Layout row wrapper — transparent (no name added to children, no
`collapsed` toggle). Distinct from `GroupField` only by the absence
of `collapsed`.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_

### TabsField

Tabbed layout wrapper — each tab carries its own sub-fields.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`tabs`** (Vec&lt;[TabPanel](#tabpanel)&gt;) _(optional)_

### TabPanel

One tab panel inside a `TabsField`.

- **`label`** (string) _(optional)_
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_
- **`error_count`** (Option&lt;integer&gt;) _(optional)_ — Number of validation errors inside this tab — emitted only when non-zero so templates can branch on presence with `{{#if error_count}}`.
- **`description`** (Option&lt;string&gt;) _(optional)_

### ArrayField

Repeating array of homogeneous rows.

At builder time, `sub_fields` carries the *template* sub-fields used to
render new rows, `rows` is `None`, and `row_count` is `0`. Enrichment
fills `rows` from the document data and updates `row_count`.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_ — Template sub-fields (used to render new-row UI).
- **`rows`** (Option&lt;Vec&lt;[ArrayRow](#arrayrow)&gt;&gt;) _(optional)_ — Concrete rows from the document (None pre-enrichment, Some post).
- **`row_count`** (integer) _(optional)_
- **`template_id`** (string) _(optional)_ — Sanitised id for use in template `id="…"` attributes.
- **`min_rows`** (Option&lt;integer&gt;) _(optional)_
- **`max_rows`** (Option&lt;integer&gt;) _(optional)_
- **`init_collapsed`** (boolean) _(optional)_
- **`add_label`** (Option&lt;string&gt;) _(optional)_
- **`label_field`** (Option&lt;string&gt;) _(optional)_

### ArrayRow

One concrete row in an `ArrayField::rows` list.

- **`index`** (integer) _(optional)_
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_
- **`has_errors`** (Option&lt;boolean&gt;) _(optional)_ — `Some(true)` when at least one sub-field has a validation error; absent otherwise.
- **`custom_label`** (Option&lt;string&gt;) _(optional)_ — Pre-computed row label (from the configured `label_field` or the `row_label` Lua hook).

### BlocksField

Repeating array of heterogeneous block-typed rows.

`block_definitions` carries the available block types and their template
sub-fields. Enrichment fills `rows` with the concrete block rows from
the document.

- **`name`** (string) _(optional)_ — Form-input name attribute / qualified data-key — the prefixed path version (e.g. `"seo__rating"` for a rating inside a group, `"items[0][rating]"` inside an array row). What the browser submits and what server-side validation keys off.
- **`field_name`** (string) _(optional)_ — Bare field name as declared on the `FieldDefinition`, without any group/array prefix. A field declared as `name = "rating"` always has `field_name == "rating"` regardless of nesting depth. Templates use this when they want to match on the "kind of field" rather than its position in the form (e.g. an overlay rendering a stars widget for any field literally named `rating`, whether it lives at the top level or inside a group).
- **`label`** (string) _(optional)_
- **`required`** (boolean) _(optional)_
- **`value`** (any) _(optional)_
- **`placeholder`** (Option&lt;string&gt;) _(optional)_
- **`description`** (Option&lt;string&gt;) _(optional)_
- **`readonly`** (boolean) _(optional)_
- **`localized`** (boolean) _(optional)_
- **`locale_locked`** (boolean) _(optional)_
- **`position`** (Option&lt;string&gt;) _(optional)_ — Where to render this field — `None` for main, `Some("sidebar")` for the right-hand sidebar.
- **`template`** (Option&lt;string&gt;) _(optional)_ — Per-field render template override — set when the field's `admin.template` is configured. Read by `RenderFieldHelper` to route to a custom template instead of the default `fields/<field_type>` lookup. Top-level (matching the flatten convention used by `label`, `placeholder`, etc.) so templates reference it as `{{template}}` rather than `{{admin.template}}`.  `RenderFieldHelper`: crate::admin::templates::helpers
- **`extra`** (Object) _(optional)_ — Freeform per-field config map — set from the field's `admin.extra`. Available to the field's render template as `{{extra.<key>}}` so a custom template can read its config without forking per field instance. Empty by default.
- **`error`** (Option&lt;string&gt;) _(optional)_ — Validation error message for this field, if any.
- **`min_length`** (Option&lt;integer&gt;) _(optional)_
- **`max_length`** (Option&lt;integer&gt;) _(optional)_
- **`min`** (Option&lt;number&gt;) _(optional)_
- **`max`** (Option&lt;number&gt;) _(optional)_
- **`has_min`** (Option&lt;boolean&gt;) _(optional)_ — Companion flag for `min` — emitted alongside the bound for templates that branch on presence. Set to `Some(true)` exactly when `min` is `Some`.
- **`has_max`** (Option&lt;boolean&gt;) _(optional)_
- **`condition_visible`** (Option&lt;boolean&gt;) _(optional)_ — Initial visibility resolved by the Lua condition function.
- **`condition_ref`** (Option&lt;string&gt;) _(optional)_ — Server-side function reference (set when the condition function returns a bool). The client re-asks the server when the form changes.
- **`condition_json`** ([ConditionExpr](#conditionexpr) \| null) _(optional)_ — Client-evaluable condition expression (set when the condition function returns a Lua table). The client evaluates this directly without a round-trip. Serializes to the same JSON shape the JS evaluator at `static/components/conditions.js` expects.
- **`block_definitions`** (Vec&lt;[BlockDefinition](#blockdefinition)&gt;) _(optional)_
- **`rows`** (Option&lt;Vec&lt;[BlockRow](#blockrow)&gt;&gt;) _(optional)_
- **`row_count`** (integer) _(optional)_
- **`template_id`** (string) _(optional)_
- **`min_rows`** (Option&lt;integer&gt;) _(optional)_
- **`max_rows`** (Option&lt;integer&gt;) _(optional)_
- **`init_collapsed`** (boolean) _(optional)_
- **`add_label`** (Option&lt;string&gt;) _(optional)_
- **`picker`** (Option&lt;string&gt;) _(optional)_ — Block picker UI style.
- **`label_field`** (Option&lt;string&gt;) _(optional)_ — Optional sub-field name used as the row label in the admin UI.

### BlockDefinition

One block-type definition inside a `BlocksField::block_definitions`
array. Carries the template sub-fields used to render a new block of
this type.

- **`block_type`** (string) _(optional)_
- **`label`** (string) _(optional)_
- **`fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_ — Template sub-fields for this block type.
- **`label_field`** (Option&lt;string&gt;) _(optional)_
- **`group`** (Option&lt;string&gt;) _(optional)_ — Optional grouping for the block picker UI.
- **`image_url`** (Option&lt;string&gt;) _(optional)_

### BlockRow

One concrete row in a `BlocksField::rows` list. Mirrors `ArrayRow`
but also carries the block discriminator (the `_block_type` JSON key,
underscore-prefixed for legacy on-the-wire compatibility).

- **`index`** (integer) _(optional)_
- **`_block_type`** (string) _(optional)_ — JSON key is `_block_type` to match the existing template contract.
- **`block_label`** (string) _(optional)_ — Display label for the block — defaults to the `block_type` when not configured. Populated by enrichment.
- **`sub_fields`** (Vec&lt;[FieldContext](#fieldcontext)&gt;) _(optional)_
- **`has_errors`** (Option&lt;boolean&gt;) _(optional)_
- **`custom_label`** (Option&lt;string&gt;) _(optional)_

### AuthCollection

One auth-enabled collection shown in the picker on the login / forgot
password forms (when more than one auth collection exists).

- **`slug`** (string)
- **`display_name`** (string)

### CollectionEntry

One row on the `/admin/collections` listing page.

- **`slug`** (string)
- **`display_name`** (string)
- **`field_count`** (integer)

### CollectionCard

One collection summary card on the dashboard. The shape mirrors the
keys the dashboard template reads.

- **`slug`** (string)
- **`display_name`** (string)
- **`singular_name`** (string)
- **`count`** (integer)
- **`last_updated`** (Option&lt;string&gt;) _(optional)_
- **`is_auth`** (boolean)
- **`is_upload`** (boolean)
- **`has_versions`** (boolean)

### GlobalCard

One global summary card on the dashboard.

- **`slug`** (string)
- **`display_name`** (string)
- **`last_updated`** (Option&lt;string&gt;) _(optional)_
- **`has_versions`** (boolean)

### UploadFormContext

Upload-collection preview block flattened onto the edit form when
`def.upload` is set.

- **`accept`** (Option&lt;string&gt;) _(optional)_ — Comma-joined accept list for the file input — emitted only when the collection declares allowed mime types.
- **`focal_x`** (Option&lt;number&gt;) _(optional)_
- **`focal_y`** (Option&lt;number&gt;) _(optional)_
- **`preview`** (Option&lt;string&gt;) _(optional)_ — Image preview URL when the file is an image.
- **`info`** ([UploadInfo](#uploadinfo) \| null) _(optional)_ — Filename + dimensions/filesize info pill.

### UploadInfo

- **`filename`** (string)
- **`filesize_display`** (Option&lt;string&gt;) _(optional)_
- **`dimensions`** (Option&lt;string&gt;) _(optional)_

