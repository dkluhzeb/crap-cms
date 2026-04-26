/**
 * ProseMirror plugin / keymap / input-rules construction.
 *
 * Pure functions of `(PM, schema, has)`. No `this` dependency.
 *
 * @module richtext/plugins
 */

/**
 * @typedef {(name: string) => boolean} FeatureCheck
 */

/**
 * Build the editor's plugin list.
 *
 * @param {any} PM ProseMirror namespace.
 * @param {any} schema
 * @param {FeatureCheck} has
 * @param {(view: any) => void} onUpdate Called on every PM transaction
 *   (used to keep the toolbar's active state in sync).
 */
export function buildPlugins(PM, schema, has, onUpdate) {
  const toolbarKey = new PM.PluginKey('toolbar');
  const toolbarPlugin = new PM.Plugin({
    key: toolbarKey,
    view: () => ({ update: (/** @type {any} */ view) => onUpdate(view) }),
  });

  return [
    PM.inputRules({ rules: buildInputRules(PM, schema, has) }),
    PM.keymap(buildListKeymap(PM, schema)),
    PM.keymap(buildMarkKeymap(PM, schema, has)),
    PM.keymap(PM.baseKeymap),
    PM.dropCursor(),
    PM.gapCursor(),
    PM.history(),
    toolbarPlugin,
  ];
}

/**
 * Smart-quote/em-dash/ellipsis rules plus block-shorthands (`>`, `1.`,
 * `*`, ` ``` `, `# `) when the corresponding feature is enabled.
 *
 * @param {any} PM
 * @param {any} schema
 * @param {FeatureCheck} has
 */
function buildInputRules(PM, schema, has) {
  const rules = [...PM.smartQuotes, PM.emDash, PM.ellipsis];
  if (has('blockquote') && schema.nodes.blockquote) {
    rules.push(PM.wrappingInputRule(/^\s*>\s$/, schema.nodes.blockquote));
  }
  if (has('orderedList') && schema.nodes.ordered_list) {
    rules.push(
      PM.wrappingInputRule(
        /^(\d+)\.\s$/,
        schema.nodes.ordered_list,
        (/** @type {RegExpExecArray} */ m) => ({ order: +m[1] }),
        (/** @type {RegExpExecArray} */ m, /** @type {any} */ node) =>
          node.childCount + node.attrs.order === +m[1],
      ),
    );
  }
  if (has('bulletList') && schema.nodes.bullet_list) {
    rules.push(PM.wrappingInputRule(/^\s*([-*])\s$/, schema.nodes.bullet_list));
  }
  if (has('codeBlock') && schema.nodes.code_block) {
    rules.push(PM.textblockTypeInputRule(/^```$/, schema.nodes.code_block));
  }
  if (has('heading') && schema.nodes.heading) {
    rules.push(
      PM.textblockTypeInputRule(
        /^(#{1,3})\s$/,
        schema.nodes.heading,
        (/** @type {RegExpExecArray} */ m) => ({ level: m[1].length }),
      ),
    );
  }
  return rules;
}

/** @param {any} PM @param {any} schema */
function buildListKeymap(PM, schema) {
  /** @type {Record<string, any>} */
  const keymap = {};
  if (schema.nodes.list_item) {
    keymap.Enter = PM.splitListItem(schema.nodes.list_item);
    keymap.Tab = PM.sinkListItem(schema.nodes.list_item);
    keymap['Shift-Tab'] = PM.liftListItem(schema.nodes.list_item);
  }
  return keymap;
}

/**
 * @param {any} PM
 * @param {any} schema
 * @param {FeatureCheck} has
 */
function buildMarkKeymap(PM, schema, has) {
  /** @type {Record<string, any>} */
  const keymap = {
    'Mod-z': PM.undo,
    'Mod-shift-z': PM.redo,
    'Mod-y': PM.redo,
  };
  if (has('bold') && schema.marks.strong) keymap['Mod-b'] = PM.toggleMark(schema.marks.strong);
  if (has('italic') && schema.marks.em) keymap['Mod-i'] = PM.toggleMark(schema.marks.em);
  if (has('code') && schema.marks.code) keymap['Mod-`'] = PM.toggleMark(schema.marks.code);
  return keymap;
}
