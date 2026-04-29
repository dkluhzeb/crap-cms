<!--
  AUTO-GENERATED — do not edit by hand.
  Source of truth: typed page-context structs in `src/admin/context/page/`.
  Regenerate with: `UPDATE_SCHEMA_DOC=1 cargo test template_context_doc_is_in_sync`
-->

# Admin template context reference

Every admin page renders a typed Rust struct serialized to JSON, runs it through the optional `before_render` Lua hook, and hands it to Handlebars. This file lists every page, its `page.type` discriminant, the template it renders, and the fields the template can rely on.

Field types use Rust-style notation: `string`, `integer`, `boolean`, `Vec<T>`, `Option<T>`. Composite leaves like `CrapMeta`, `NavData`, `FieldContext` link into the [shared definitions](#shared-definitions) section at the bottom.

## Login page

- **`page.type`**: `auth_login`
- **Template**: `templates/auth/login.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec<string>)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`error`** (Option<string>) _(optional)_ — Error key (e.g., `"error_invalid_credentials"`) — present after a failed login post.
- **`email`** (Option<string>) _(optional)_ — Pre-fills the email field after a failed login.
- **`collections`** (Vec<[AuthCollection](#authcollection)>)
- **`show_collection_picker`** (boolean)
- **`disable_local`** (boolean)
- **`show_forgot_password`** (boolean)
- **`success`** (Option<string>) _(optional)_ — Whitelisted success-message key shown after redirect from logout / email verification / password reset. Always emitted (as `null` when absent) to preserve the original `Option`-as-null contract.

## MFA challenge page

- **`page.type`**: `auth_mfa`
- **Template**: `templates/auth/mfa.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec<string>)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`error`** (Option<string>) _(optional)_

## Forgot password page

- **`page.type`**: `auth_forgot`
- **Template**: `templates/auth/forgot_password.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec<string>)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`success`** (boolean)
- **`collections`** (Vec<[AuthCollection](#authcollection)>)
- **`show_collection_picker`** (boolean)

## Reset password page

- **`page.type`**: `auth_reset`
- **Template**: `templates/auth/reset_password.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec<string>)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))
- **`token`** (Option<string>) _(optional)_ — Token from the URL — present only when valid. Absent when the link is bad / expired (in which case `error` is set instead).
- **`error`** (Option<string>) _(optional)_

## Error pages (403 / 404 / 500)

- **`page.type`**: `error_403 | error_404 | error_500`
- **Template**: `templates/errors/{403,404,500}.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`message`** (string) — User-facing error message body.

## Dashboard

- **`page.type`**: `dashboard`
- **Template**: `templates/dashboard/index.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection_cards`** (Vec<[CollectionCard](#collectioncard)>)
- **`global_cards`** (Vec<[GlobalCard](#globalcard)>)

## Collection list

- **`page.type`**: `collection_list`
- **Template**: `templates/collections/list.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collections`** (Vec<[CollectionEntry](#collectionentry)>)

## Collection items list

- **`page.type`**: `collection_items`
- **Template**: `templates/collections/items.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`docs`** (Vec<any>)
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`has_drafts`** (boolean)
- **`has_soft_delete`** (boolean)
- **`is_trash`** (boolean)
- **`search`** (Option<string>) _(optional)_
- **`sort`** (Option<string>) _(optional)_
- **`table_columns`** (Vec<any>)
- **`column_options`** (Vec<any>)
- **`filter_fields`** (Vec<any>)
- **`active_filters`** (Vec<any>)
- **`active_filter_count`** (integer)
- **`title_sort_url`** (Option<string>) _(optional)_
- **`title_sorted_asc`** (boolean)
- **`title_sorted_desc`** (boolean)

## Collection edit form

- **`page.type`**: `collection_edit`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref))
- **`fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`sidebar_fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** (Vec<any>)
- **`has_more_versions`** (boolean)
- **`restore_url_prefix`** (string)
- **`versions_url`** (string)
- **`document_title`** (string)
- **`ref_count`** (integer)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec<[LocaleTemplateOption](#localetemplateoption)>) _(optional)_
- **`upload`** ([UploadFormContext](#uploadformcontext) \| null) _(optional)_ — Upload preview block — present only on upload collections.

## Collection create form

- **`page.type`**: `collection_create`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`sidebar_fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec<[LocaleTemplateOption](#localetemplateoption)>) _(optional)_
- **`upload`** ([UploadFormContext](#uploadformcontext) \| null) _(optional)_

## Collection form-error re-render

- **`page.type`**: `collection_edit | collection_create`
- **Template**: `templates/collections/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref) \| null) _(optional)_ — Document stub (with `id` only) on edit error; absent on create error.
- **`fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`sidebar_fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`editing`** (boolean)
- **`has_drafts`** (boolean)
- **`upload_hidden_fields`** (Option<Vec<any>>) _(optional)_ — Hidden upload fields preserved from the submitted form (edit-mode upload errors only, so the user keeps their pending file metadata).

## Collection delete confirmation

- **`page.type`**: `collection_delete`
- **Template**: `templates/collections/delete.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document_id`** (string)
- **`title_value`** (Option<string>) _(optional)_ — Document title for display. `None` (serialized as `null`) when the collection has no title field or the read fell through.
- **`ref_count`** (integer)

## Collection versions list

- **`page.type`**: `collection_versions`
- **Template**: `templates/collections/versions.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref))
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`doc_title`** (string)
- **`versions`** (Vec<any>)
- **`restore_url_prefix`** (string)

## Collection restore confirmation

- **`page.type`**: `collection_versions`
- **Template**: `templates/collections/restore.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`collection`** ([CollectionContext](#collectioncontext))
- **`document`** ([DocumentRef](#documentref))
- **`version_number`** (any) — Version number being restored (from the version row's `version` column).
- **`missing_relations`** (Vec<any>) — IDs of relationship references whose targets no longer exist.
- **`restore_url`** (string)
- **`back_url`** (string)

## Global edit form

- **`page.type`**: `global_edit`
- **Template**: `templates/globals/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`sidebar_fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** (Vec<any>)
- **`has_more_versions`** (boolean)
- **`restore_url_prefix`** (string)
- **`versions_url`** (string)
- **`doc_status`** (string)
- **`has_locales`** (boolean) _(optional)_
- **`current_locale`** (string) _(optional)_
- **`locales`** (Vec<[LocaleTemplateOption](#localetemplateoption)>) _(optional)_

## Global form-error re-render

- **`page.type`**: `global_edit`
- **Template**: `templates/globals/edit.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`fields`** (Vec<[FieldContext](#fieldcontext)>)
- **`sidebar_fields`** (Vec<[FieldContext](#fieldcontext)>)

## Global versions list

- **`page.type`**: `global_versions`
- **Template**: `templates/globals/versions.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`pagination`** ([PaginationContext](#paginationcontext))
- **`versions`** (Vec<any>)
- **`restore_url_prefix`** (string)

## Global restore confirmation

- **`page.type`**: `global_versions`
- **Template**: `templates/globals/restore.hbs`

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_
- **`global`** ([GlobalContext](#globalcontext))
- **`version_number`** (any)
- **`missing_relations`** (Vec<any>)
- **`restore_url`** (string)
- **`back_url`** (string)


---

## Shared definitions

Every page above flattens [BasePageContext](#basepagecontext) (or [AuthBasePageContext](#authbasepagecontext) for auth-flow pages) into its top-level fields. The base structs and their leaves are defined here once.

### BasePageContext

- **`crap`** ([CrapMeta](#crapmeta))
- **`nav`** ([NavData](#navdata))
- **`user`** ([UserContext](#usercontext) \| null) _(optional)_
- **`_locale`** (string) — Active UI translation locale.
- **`available_locales`** (Vec<string>) — Available UI translation locales (for the locale picker).
- **`title`** (string) — Page title — duplicated at top level for backward compat with the base layout that reads `{{title}}` directly. Templates that have migrated read `{{page.title}}` instead.
- **`page`** ([PageMeta](#pagemeta))
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb partial prefers `page.breadcrumbs` and falls back to this. Kept for backward compat with overridden templates.
- **`has_editor_locales`** (Option<boolean>) _(optional)_
- **`editor_locale`** (Option<string>) _(optional)_
- **`editor_locales`** (Option<Vec<[EditorLocaleOption](#editorlocaleoption)>>) _(optional)_

### AuthBasePageContext

- **`crap`** ([CrapMeta](#crapmeta))
- **`_locale`** (string)
- **`available_locales`** (Vec<string>)
- **`title`** (string)
- **`page`** ([PageMeta](#pagemeta))

### PageMeta

- **`type`** (string) — Page-type discriminant. Serialized as a snake_case string literal so templates can branch with `{{#if (eq page.type "collection_edit")}}`.
- **`title`** (string) — Page title or translation key.
- **`title_name`** (Option<string>) _(optional)_ — Optional interpolation param for `{{t page.title name=page.title_name}}`.
- **`breadcrumbs`** (Vec<[Breadcrumb](#breadcrumb)>) — Breadcrumb trail rendered by `partials/breadcrumb.hbs`.

### CrapMeta

- **`version`** (string) — Crate version (Cargo.toml `version`).
- **`build_hash`** (string) — Build hash (set by build script from git).
- **`dev_mode`** (boolean) — Whether admin dev-mode is enabled (per-request template reload, etc.).
- **`auth_enabled`** (boolean) — Whether the system has any auth-enabled collections.
- **`csp_nonce`** (string) — Per-request CSP nonce (empty string outside request scope).

### NavData

- **`collections`** (Vec<[NavCollection](#navcollection)>)
- **`globals`** (Vec<[NavGlobal](#navglobal)>)
- **`custom_pages`** (Vec<[CustomPage](#custompage)>) — Filesystem-routed custom admin pages registered via `crap.pages.register`. Only entries with a `label` set appear here.

### NavCollection

- **`slug`** (string)
- **`display_name`** (string)
- **`is_auth`** (boolean)
- **`is_upload`** (boolean)

### NavGlobal

- **`slug`** (string)
- **`display_name`** (string)

### UserContext

- **`email`** (string)
- **`id`** (string)
- **`collection`** (string)

### EditorLocaleOption

- **`value`** (string)
- **`label`** (string)
- **`selected`** (boolean)

### LocaleTemplateOption

- **`value`** (string)
- **`label`** (string)
- **`selected`** (boolean)

### Breadcrumb

- **`label`** (string) — The text label to display for the breadcrumb.
- **`url`** (Option<string>) _(optional)_ — The optional URL to link to. If None, the breadcrumb is the current page.
- **`label_name`** (Option<string>) _(optional)_ — Optional interpolation param for `{{t label name=label_name}}`.

### CollectionContext

- **`slug`** (string)
- **`display_name`** (string)
- **`singular_name`** (string)
- **`title_field`** (Option<string>) _(optional)_
- **`timestamps`** (boolean)
- **`is_auth`** (boolean)
- **`is_upload`** (boolean)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`soft_delete`** (boolean)
- **`can_permanently_delete`** (boolean)
- **`admin`** ([AdminMeta](#adminmeta))
- **`upload`** ([UploadMeta](#uploadmeta) \| null) _(optional)_
- **`versions`** ([VersionsMeta](#versionsmeta) \| null) _(optional)_
- **`auth`** ([AuthMeta](#authmeta) \| null) _(optional)_
- **`fields_meta`** (Vec<[FieldMeta](#fieldmeta)>)

### GlobalContext

- **`slug`** (string)
- **`display_name`** (string)
- **`has_drafts`** (boolean)
- **`has_versions`** (boolean)
- **`versions`** ([VersionsMeta](#versionsmeta) \| null) _(optional)_
- **`fields_meta`** (Vec<[FieldMeta](#fieldmeta)>)

### DocumentRef

- **`id`** (string)
- **`created_at`** (Option<string>) _(optional)_
- **`updated_at`** (Option<string>) _(optional)_
- **`status`** (Option<string>) _(optional)_
- **`data`** (Option<Object>) _(optional)_

### PaginationContext

- **`per_page`** (integer)
- **`total`** (integer)
- **`has_prev`** (boolean)
- **`has_next`** (boolean)
- **`prev_url`** (string)
- **`next_url`** (string)
- **`page`** (Option<integer>) _(optional)_ — Page-mode only — current page number (1-indexed).
- **`total_pages`** (Option<integer>) _(optional)_ — Page-mode only — total page count.

### FieldContext

_(No fields.)_

### AuthCollection

- **`slug`** (string)
- **`display_name`** (string)

### CollectionEntry

- **`slug`** (string)
- **`display_name`** (string)
- **`field_count`** (integer)

### CollectionCard

- **`slug`** (string)
- **`display_name`** (string)
- **`singular_name`** (string)
- **`count`** (integer)
- **`last_updated`** (Option<string>) _(optional)_
- **`is_auth`** (boolean)
- **`is_upload`** (boolean)
- **`has_versions`** (boolean)

### GlobalCard

- **`slug`** (string)
- **`display_name`** (string)
- **`last_updated`** (Option<string>) _(optional)_
- **`has_versions`** (boolean)

### UploadFormContext

- **`accept`** (Option<string>) _(optional)_ — Comma-joined accept list for the file input — emitted only when the collection declares allowed mime types.
- **`focal_x`** (Option<number>) _(optional)_
- **`focal_y`** (Option<number>) _(optional)_
- **`preview`** (Option<string>) _(optional)_ — Image preview URL when the file is an image.
- **`info`** ([UploadInfo](#uploadinfo) \| null) _(optional)_ — Filename + dimensions/filesize info pill.

### UploadInfo

- **`filename`** (string)
- **`filesize_display`** (Option<string>) _(optional)_
- **`dimensions`** (Option<string>) _(optional)_

