# Source 3 census: Nuxt UI v4 component surface

Census of every component Nuxt UI v4 ships, its overlap with Reka UI, and its
theming mechanism. Census document only. It contains no Vyrn code and makes no
design decision.

## Method and citation bases

Component inventory comes from the docs sitemap
(<https://ui.nuxt.com/sitemap.md>, 131 component pages) cross-checked against
the repo directory `src/runtime/components`
(commit `aa5f4af081428fc190149985adfb13c953f4d35f`,
<https://github.com/nuxt/ui/tree/aa5f4af081428fc190149985adfb13c953f4d35f/src/runtime/components>).
Nuxt UI states it is built on Reka UI, Tailwind CSS, and Tailwind Variants
(<https://ui.nuxt.com/docs/getting-started>).

Shorthand used throughout:

- `[src/<file>]` = `https://github.com/nuxt/ui/blob/aa5f4af081428fc190149985adfb13c953f4d35f/<file>`
- `[doc/<slug>]` = `https://ui.nuxt.com/docs/components/<slug>`
- `[R:<slug>]` = `https://reka-ui.com/docs/components/<slug>`
- `P1` = the controlled/uncontrolled contradictory pair. The component accepts
  a controlled value (`modelValue`, `open`, `page`, or `expanded`) AND an
  uncontrolled initial value (`defaultValue`, `defaultOpen`, `defaultPage`, or
  `defaultExpanded`). Both can be set at once, which gives two sources of truth
  for one value. Verified per component against its props interface; examples:
  [src/src/runtime/components/Accordion.vue] line 38,
  [src/src/runtime/components/Collapsible.vue] line 10,
  [src/src/runtime/components/Pagination.vue] line 12,
  [src/src/runtime/components/Tree.vue] line 34,
  [src/src/runtime/components/Input.vue] line 95.
- "Primitive-only" = the component imports nothing from Reka UI except
  `Primitive` (the polymorphic element renderer), `Slot`, or similar utilities.
  No Reka behavior primitive sits underneath it.

The accessibility contract column names the contract the docs promise. For
Reka-wrapped components the contract comes from the named Reka primitive; the
wrapper forwards its props ([src/src/runtime/composables/useForwardProps.ts]).
Where the wrapper adds nothing visible beyond the primitive, the row says so.
Unverified specifics are marked NOT VERIFIED.

## Component table

131 documented components, alphabetical by docs slug.

| component | what it is | the accessibility contract it promises | the DOM structure it requires | the state it owns | the props that can be set to a contradictory pair | overlaps reka-ui |
|---|---|---|---|---|---|---|
| Accordion | Stacked set of collapsible panels [doc/accordion]. | Header buttons toggle regions; keyboard nav from Reka Accordion [R:accordion]. | Single root; panel content comes from the `items` prop or slots. | Open-item state (`modelValue`), `multiple` flag. | P1 (`defaultValue` + `modelValue`). | Wraps `AccordionRoot/Item/Header/Trigger/Content` [src/src/runtime/components/Accordion.vue]. |
| Alert | Callout to draw attention [doc/alert]. | None promised beyond text content. | One root element; icon, title, description slots. | None (dismissal is an event, not state). | none found. | Primitive-only [src/src/runtime/components/Alert.vue]. |
| App | App-level provider for config, locale, tooltips, portals [doc/app]. | Provides tooltip provider context for descendants. | Must wrap the application once. | Portal target, theme context provision. | none found. | Wraps Reka `ConfigProvider`, `TooltipProvider` [src/src/runtime/components/App.vue]. |
| AuthForm | Prebuilt login/register/reset form [doc/auth-form]. | Inherits UForm validation flow; fields labeled via UFormField. | Composition: UForm + UFormField children. | `email`, `password`, `remember` reactive form state; submit/validate status. | `validateOn` event list vs explicit `validate()` call timing (two triggers, not two values): none found as value pairs. | Composes UForm/UFormField/UButton; Primitive-only itself [src/src/runtime/components/AuthForm.vue]. |
| Avatar | Image with fallback [doc/avatar]. | `alt` text on the img; fallback icon/text when src fails. | Single root; img plus optional fallback layer. | Internal `error` flag after image load failure. | `icon` vs `text` fallback source when `src` fails (two fallbacks defined): none found as error. | Primitive-only [src/src/runtime/components/Avatar.vue]. |
| AvatarGroup | Stacks avatars [doc/avatar-group]. | Same as children avatars; group size propagates. | Wraps UAvatar children. | Group size provide/inject. | none found. | Primitive-only [src/src/runtime/components/AvatarGroup.vue]. |
| Badge | Short status text [doc/badge]. | None promised; plain rendered text. | One root element. | None. | none found. | Primitive-only [src/src/runtime/components/Badge.vue]. |
| Banner | Top-of-page announcement bar [doc/banner]. | Close button is a labeled UButton. | Fixed-position root above page content. | `open` dismiss state. | none found. | Primitive-only [src/src/runtime/components/Banner.vue]. |
| BlogPost | Article card [doc/blog-post]. | Title renders as link text. | Root article/card element. | None. | `to` link vs click handler: none found. | Primitive-only [src/src/runtime/components/BlogPost.vue]. |
| BlogPosts | Responsive grid of BlogPost cards [doc/blog-posts]. | Inherits child card contracts. | Grid root of BlogPost children. | None. | none found. | Primitive-only [src/src/runtime/components/BlogPosts.vue]. |
| Breadcrumb | Hierarchy of links [doc/breadcrumb]. | Nav semantics expected for breadcrumbs NOT VERIFIED in source; items render as links with separators. | Root list of link items from `items`. | `expandedItems` overflow state. | `items` prop vs default slot children (two content sources). | Primitive-only [src/src/runtime/components/Breadcrumb.vue]. |
| Button | Action button or link [doc/button]. | Native `<button>` or `<a>`; `disabled`/`aria-disabled` styles built into theme classes [src/src/theme/button.ts]; focus-visible ring in compound variants [doc/theme/components]. | Single root; label, leading, trailing slots. | `loading` flag (also auto-derived from `@click` promise when `loadingAuto`). | P1 variant: `loading` set manually while `loadingAuto` derives loading from the click promise (two sources for the same state) [src/src/runtime/components/Button.vue]. | New. Renders through ULink; no Reka primitive [src/src/runtime/components/Button.vue]. |
| Calendar | Date picker for single, multiple, range [doc/calendar]. | Grid/cell roles and arrow-key navigation from Reka Calendar [R:calendar]. | Popup-free grid; month/year controls optional. | Selected date(s), view month/year. | P1 (`defaultValue` + `modelValue`); also `range` switches between forwarded Reka calendar kinds. | Wraps Reka `Calendar`, `RangeCalendar`, `MonthPicker`, `YearPicker` (namespaced) [src/src/runtime/components/Calendar.vue]. |
| Card | Content card with header, body, footer [doc/card]. | None promised. | Three nested slot divs (header/body/footer) [doc/theme/components shows template]. | None. | none found. | Primitive-only [src/src/runtime/components/Card.vue]. |
| Carousel | Embla-based swipe carousel [doc/carousel]. | Slide semantics from Embla markup NOT VERIFIED; arrows/dots are Buttons. | Viewport div containing track div of slides. | Embla API instance; selected index; plugin options. | `autoplay` plugin vs `autoScroll` plugin both configurable: potential conflict NOT VERIFIED. | Embla Carousel + Primitive-only [src/src/runtime/components/Carousel.vue], [https://www.embla-carousel.com]. |
| ChangelogVersion | Changelog article card [doc/changelog-version]. | Title/date as link text. | Root card element. | None. | none found. | Primitive-only [src/src/runtime/components/ChangelogVersion.vue]. |
| ChangelogVersions | Timeline list of changelog versions [doc/changelog-versions]. | Inherits child contracts. | List of ChangelogVersion children. | None. | none found. | Primitive-only [src/src/runtime/components/ChangelogVersions.vue]. |
| Chat | Docs overview page for the chat family [doc/chat]. | Sum of family members. | Family usage pattern, not a shipped .vue file. | Not applicable (no Chat.vue in `src/runtime/components`). | Not applicable. | Not applicable. |
| ChatMessage | One chat message with avatar and actions [doc/chat-message]. | None promised beyond content. | Message row with side slot. | Hover/participant context via injection. | none found. | Primitive-only [src/src/runtime/components/ChatMessage.vue]. |
| ChatMessages | Scrolling message list with autoscroll [doc/chat-messages]. | Autoscroll region NOT VERIFIED for live-region roles. | Viewport div of ChatMessage children. | Auto-scroll on/off state. | none found. | Uses Reka `Presence`; otherwise new [src/src/runtime/components/ChatMessages.vue]. |
| ChatPalette | Chatbot interface inside an overlay palette [doc/chat-palette]. | Dialog/listbox semantics depend on overlay parent. | Overlay body with prompt area. | Messages, open state via overlay. | none found. | Primitive-only [src/src/runtime/components/ChatPalette.vue]. |
| ChatPrompt | Enhanced textarea for prompts [doc/chat-prompt]. | Textarea labeling inherited from input patterns. | Single textarea root with slots. | Prompt text model. | none found. | Primitive-only [src/src/runtime/components/ChatPrompt.vue]. |
| ChatPromptSubmit | Submit button with status handling [doc/chat-prompt-submit]. | Native button. | Single button. | Loading/status derived from prompt state. | none found. | New; composes UButton [src/src/runtime/components/ChatPromptSubmit.vue]. |
| ChatReasoning | Collapsible reasoning display [doc/chat-reasoning]. | Disclosure semantics from Reka Collapsible [R:collapsible]. | Trigger plus collapsible content. | `open` state. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/ChatReasoning.vue] line 12. | Wraps `CollapsibleRoot/Trigger/Content` [src/src/runtime/components/ChatReasoning.vue]. |
| ChatShimmer | Animated placeholder text [doc/chat-shimmer]. | None promised. | Single span. | Animation phase. | none found. | Primitive-only [src/src/runtime/components/ChatShimmer.vue]. |
| ChatTool | Collapsible tool-invocation display [doc/chat-tool]. | Disclosure semantics from Reka Collapsible [R:collapsible]. | Trigger plus collapsible content. | `open` state. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/ChatTool.vue] line 13. | Wraps `CollapsibleRoot/Trigger/Content` [src/src/runtime/components/ChatTool.vue]. |
| Checkbox | Toggle between checked and unchecked [doc/checkbox]. | `role=checkbox`, `aria-checked`, Space toggles; label association via Reka `Label` [R:checkbox]. | Hidden native input plus visual control; optional label. | Checked state (`trueValue`/`falseValue`), indeterminate. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/Checkbox.vue] line 12. | Wraps `CheckboxRoot/Indicator` + `Label` [src/src/runtime/components/Checkbox.vue]. |
| CheckboxGroup | Multiple checkboxes from items [doc/checkbox-group]. | Group + checkbox roles from Reka CheckboxGroup [R:checkbox]. | Group root rendering Checkbox children from `items`. | Array checked state. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/CheckboxGroup.vue] line 112. | Wraps `CheckboxGroupRoot` [src/src/runtime/components/CheckboxGroup.vue]. |
| Chip | Numeric/state indicator badge on a trigger [doc/chip]. | Indicator text; show/hide state announced NOT VERIFIED. | Trigger element with absolutely positioned indicator. | `show` flag. | none found. | Primitive-only [src/src/runtime/components/Chip.vue]. |
| Collapsible | Toggle visibility of content [doc/collapsible]. | Trigger button with disclosure semantics from Reka Collapsible [R:collapsible]. | Root with trigger and content regions. | `open` state. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/Collapsible.vue] line 10. | Wraps `CollapsibleRoot/Trigger/Content` [src/src/runtime/components/Collapsible.vue]. |
| ColorModeAvatar | Avatar swapping source by color mode [doc/color-mode-avatar]. | Same as Avatar. | Single Avatar root. | Reads color mode; no owned state. | none found. | Composes UAvatar; Primitive-only [src/src/runtime/components/color-mode/ColorModeAvatar.vue]. |
| ColorModeButton | Light/dark toggle button [doc/color-mode-button]. | Native button; icon reflects mode. | Single button. | Reads/writes color mode preference. | none found. | Composes UButton; Primitive-only [src/src/runtime/components/color-mode/ColorModeButton.vue]. |
| ColorModeImage | Image swapping source by color mode [doc/color-mode-image]. | `alt` on img. | Two imgs, one hidden per mode NOT VERIFIED. | Reads color mode. | none found. | Primitive-only [src/src/runtime/components/color-mode/ColorModeImage.vue]. |
| ColorModeSelect | System/dark/light select [doc/color-mode-select]. | Select semantics from USelect. | Wraps USelect. | Reads/writes color mode preference. | none found. | Composes USelect [src/src/runtime/components/color-mode/ColorModeSelect.vue]. |
| ColorModeSwitch | Light/dark switch [doc/color-mode-switch]. | Switch semantics from USwitch. | Wraps USwitch. | Reads/writes color mode preference. | none found. | Composes USwitch [src/src/runtime/components/color-mode/ColorModeSwitch.vue]. |
| ColorPicker | Color selection component [doc/color-picker]. | Channel slider and text-input semantics NOT VERIFIED in wrapper. | Swatch area plus channel inputs. | Color string model. | none found. | Primitive-only; custom implementation [src/src/runtime/components/ColorPicker.vue]. |
| CommandPalette | Searchable command list with Fuse.js fuzzy matching [doc/command-palette]. | Listbox/option roles, typeahead, arrow navigation from Reka Listbox [R:listbox]. | Filter input above virtualized item list. | Search term, selection, groups. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/CommandPalette.vue] line 69. | Wraps `ListboxRoot/Filter/Content/Group/Virtualizer/Item/ItemIndicator` [src/src/runtime/components/CommandPalette.vue]. |
| Container | Centers and constrains width [doc/container]. | None promised. | Single div. | None. | none found. | Primitive-only [src/src/runtime/components/Container.vue]. |
| ContentNavigation | Accordion-style navigation from Nuxt Content [doc/content-navigation]. | Disclosure semantics inherited from UNavigationMenu/UAccordion composition NOT VERIFIED. | Tree of links. | Expanded sections. | none found. | Composes other Nuxt UI components [src/src/runtime/components/content/ContentNavigation.vue]. |
| ContentSearch | Ready-made CommandPalette for docs search [doc/content-search]. | CommandPalette contract. | Modal hosting CommandPalette. | Modal open state, query. | none found. | Composes UModal + UCommandPalette [src/src/runtime/components/content/ContentSearch.vue]. |
| ContentSearchButton | Styled button opening ContentSearch [doc/content-search-button]. | Native button/link. | Single button. | none found beyond button state. | none found. | Composes UButton + UContentSearch [src/src/runtime/components/content/ContentSearchButton.vue]. |
| ContentSurround | Prev/next page links [doc/content-surround]. | Two links. | Pair of link cards. | None. | none found. | Composes UButton/ULink [src/src/runtime/components/content/ContentSurround.vue]. |
| ContentToc | Sticky table of contents with active highlight [doc/content-toc]. | Links list; active anchor tracked visually. | Nested link list. | Active heading id (scroll spy). | none found. | Primitive-only [src/src/runtime/components/content/ContentToc.vue]. |
| ContextMenu | Actions on right-click [doc/context-menu]. | Menu/menuitem roles, arrow navigation, typeahead from Reka ContextMenu [R:context-menu]. | Trigger host plus portal content. | `open` state, search term. | P1 (`open` + `defaultOpen` forwarded through Reka menu root props) [src/src/runtime/components/ContextMenu.vue]. | Wraps `ContextMenuRoot/Trigger` [src/src/runtime/components/ContextMenu.vue]. |
| ContextMenuContent | Standalone menu content half [doc/context-menu]. | Same as ContextMenu content. | Portal content element. | Forwarded menu state. | none found. | Wraps namespaced Reka `ContextMenu` content [src/src/runtime/components/ContextMenuContent.vue]. |
| DashboardGroup | Fixed layout providing dashboard sidebar context and persistence [doc/dashboard-group]. | Landmark roles NOT VERIFIED. | Wraps the dashboard layout tree. | Sidebar width/collapsed state persisted to storage. | `persistent` off while `storageKey` set: persistence flags overlap; none found as strict value contradiction. | Primitive-only [src/src/runtime/components/DashboardGroup.vue]. |
| DashboardNavbar | Dashboard top navbar [doc/dashboard-navbar]. | None promised. | Bar with left/right/default slots and toggle integration. | none found beyond layout. | none found. | Primitive-only [src/src/runtime/components/DashboardNavbar.vue]. |
| DashboardPanel | Resizable dashboard panel [doc/dashboard-panel]. | Resize handle keyboard support depends on handle implementation NOT VERIFIED. | Panel shell wrapping resize handle and content. | Size state via `useResizable`. | none found. | New; composes UDashboardResizeHandle [src/src/runtime/components/DashboardPanel.vue]. |
| DashboardResizeHandle | Handle to resize sidebar/panel [doc/dashboard-resize-handle]. | Pointer-driven; keyboard resizing NOT VERIFIED. | Single draggable element. | Drag state via `useResizable`. | none found. | Primitive-only; custom pointer logic [src/src/runtime/components/DashboardResizeHandle.vue]. |
| DashboardSearch | CommandPalette modal for dashboards [doc/dashboard-search]. | CommandPalette + modal contracts composed. | Modal hosting CommandPalette. | `open` state, groups, fuse index. | none found. | Composes UModal + UCommandPalette [src/src/runtime/components/DashboardSearch.vue]. |
| DashboardSearchButton | Button opening DashboardSearch [doc/dashboard-search-button]. | Native button/link. | Single button. | none found. | none found. | Composes UButton [src/src/runtime/components/DashboardSearchButton.vue]. |
| DashboardSidebar | Resizable, collapsible dashboard sidebar [doc/dashboard-sidebar]. | Mobile drawer / desktop aside modes carry the respective overlay contracts. | Sidebar shell with resizable body; mobile swaps to drawer/slideover/modal. | `open`, `collapsed` models; persisted size. | `collapsed` vs persisted width from `storageKey` context (two sources for effective sidebar size) NOT VERIFIED as runtime bug. | New; composes UDrawer/UButton [src/src/runtime/components/DashboardSidebar.vue]. |
| DashboardSidebarCollapse | Desktop collapse button [doc/dashboard-sidebar-collapse]. | Native button. | Single button bound to group context. | Reads/writes group collapsed state. | none found. | New; composes UButton [src/src/runtime/components/DashboardSidebarCollapse.vue]. |
| DashboardSidebarToggle | Mobile sidebar toggle [doc/dashboard-sidebar-toggle]. | Native button. | Single button bound to group context. | Reads/writes group open state. | none found. | New; composes UButton [src/src/runtime/components/DashboardSidebarToggle.vue]. |
| DashboardToolbar | Toolbar under navbar [doc/dashboard-toolbar]. | None promised. | Single bar. | None. | none found. | Primitive-only [src/src/runtime/components/DashboardToolbar.vue]. |
| Drawer | Sliding drawer (mobile sheets) [doc/drawer]. | Overlay/dialog-style dismissal; exact ARIA from vaul-vue NOT VERIFIED. | Portal overlay with handle and content. | `open` state, snap point. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/Drawer.vue] line 15. | Built on vaul-vue `DrawerRoot` [https://www.npmjs.com/package/vaul-vue]; uses Reka `VisuallyHidden` only [src/src/runtime/components/Drawer.vue]. |
| DropdownMenu | Actions on click [doc/dropdown-menu]. | Menu/menuitem roles, arrow navigation, typeahead, Esc close from Reka DropdownMenu [R:dropdown-menu]. | Trigger plus portal content. | `open` state, search term. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/DropdownMenu.vue] line 176. | Wraps `DropdownMenuRoot/Trigger/Arrow` [src/src/runtime/components/DropdownMenu.vue]. |
| DropdownMenuContent | Standalone content half [doc/dropdown-menu]. | Same menu contract. | Portal content with optional filter input. | Forwarded menu state. | none found. | Wraps namespaced Reka `DropdownMenu` content [src/src/runtime/components/DropdownMenuContent.vue]. |
| Editor | Rich text editor based on TipTap [doc/editor]. | Editing surface semantics come from TipTap NOT VERIFIED; toolbar buttons are UButtons. | Editor content area plus menu slots. | Document model (markdown/HTML/JSON), editor instance. | `model-value` vs typed `content` initialization sources NOT VERIFIED. | TipTap-based; Primitive-only [src/src/runtime/components/Editor.vue], [https://tiptap.dev]. |
| EditorDragHandle | Block drag handle [doc/editor-drag-handle]. | Drag affordance; keyboard alternative NOT VERIFIED. | Floating handle positioned via floating-ui. | Drag state. | none found. | New; uses @floating-ui/dom [src/src/runtime/components/EditorDragHandle.vue]. |
| EditorEmojiMenu | Emoji suggestions on typing `:` [doc/editor-emoji-menu]. | Popup list semantics NOT VERIFIED. | Suggestion popup bound to editor. | Query and filtered emoji list. | none found. | New; composes UCommandPalette internals [src/src/runtime/components/EditorEmojiMenu.vue]. |
| EditorMentionMenu | User mention suggestions [doc/editor-mention-menu]. | Popup list semantics NOT VERIFIED. | Suggestion popup bound to editor. | Query and filtered users. | none found. | New [src/src/runtime/components/EditorMentionMenu.vue]. |
| EditorSuggestionMenu | Slash-command suggestions [doc/editor-suggestion-menu]. | Popup list semantics NOT VERIFIED. | Suggestion popup bound to editor. | Query and filtered commands. | none found. | New [src/src/runtime/components/EditorSuggestionMenu.vue]. |
| EditorToolbar | Fixed/bubble/floating toolbar [doc/editor-toolbar]. | Toolbar buttons expose pressed state via TipTap NOT VERIFIED. | Toolbar container of buttons; includes separators. | Active marks from editor instance. | `mode` values select different positioning behaviors (mutually exclusive, not contradictory). | Uses Reka `Separator` + Primitive [src/src/runtime/components/EditorToolbar.vue]. |
| Empty | Empty-state display [doc/empty]. | None promised. | Icon/title/description/content stack. | None. | none found. | Primitive-only [src/src/runtime/components/Empty.vue]. |
| Error | Error page with NuxtError support [doc/error]. | Status and message rendered as text. | Full-page layout. | Reads Nuxt error object. | none found. | Primitive-only [src/src/runtime/components/Error.vue]. |
| FieldGroup | Groups button-like inputs [doc/field-group]. | Visual grouping only. | Wrapper injecting size/orientation context. | Provide/inject context. | none found. | Primitive-only [src/src/runtime/components/FieldGroup.vue]. |
| FileUpload | File upload input with drag-drop [doc/file-upload]. | Hidden accessible input via Reka `VisuallyHidden`; drop zone announcements NOT VERIFIED. | Dropzone plus file list. | Files model, drag-over flag. | none found. | Primitive-only; uses Reka `VisuallyHidden` [src/src/runtime/components/FileUpload.vue]. |
| Footer | Responsive site footer [doc/footer]. | None promised. | Root with left/right/default slots. | None. | none found. | Primitive-only [src/src/runtime/components/Footer.vue]. |
| FooterColumns | Link columns for footer [doc/footer-columns]. | Links list. | Column layout of link lists. | None. | none found. | Primitive-only [src/src/runtime/components/FooterColumns.vue]. |
| Form | Form with schema validation and submission [doc/form]. | Native `<form>` submit; error association handled by UFormField ids. | Wraps field elements; provides form bus. | Errors map, submitting/loading state, touched inputs. | `schema` vs `validate` function (two validation sources for one form). | New; no Reka import [src/src/runtime/components/Form.vue]. |
| FormField | Field wrapper with label, hint, error [doc/form-field]. | Label association via Reka `Label`; error linked to input id. | Label/hint/error layout around a control. | Error state from parent form bus. | `help` vs `error` display slots: none found. | Wraps Reka `Label` [src/src/runtime/components/FormField.vue]. |
| Header | Responsive site header [doc/header]. | Mobile menu toggle exposes open state; panel semantics NOT VERIFIED. | Bar plus togglable panel. | `open` model. | none found. | Primitive-only [src/src/runtime/components/Header.vue]. |
| Icon | Iconify icon via @nuxt/icon [doc/icon]. | Decorative by default; alt/label handling per @nuxt/icon. | Single svg/span. | None. | none found. | New; wraps @nuxt/icon [src/src/runtime/components/Icon.vue]. |
| Input | Text input [doc/input]. | Native `<input>`; label via UFormField. | Single input root with leading/trailing slots. | Value model. | P1 (`defaultValue` + `modelValue` resolved by useVModel) [src/src/runtime/components/Input.vue] line 95. | Primitive-only [src/src/runtime/components/Input.vue]. |
| InputDate | Date entry input [doc/input-date]. | Segmented date-field semantics from Reka DateField [R:date-field]. | Segmented input; optionally paired with UCalendar popover. | Date or range model. | P1 (`defaultValue` + `modelValue`); `range` picks DateField vs DateRangeField [src/src/runtime/components/InputDate.vue]. | Wraps namespaced Reka `DateField`/`DateRangeField` [src/src/runtime/components/InputDate.vue]. |
| InputMenu | Autocomplete input with suggestions [doc/input-menu]. | Combobox input + listbox popup roles from Reka Combobox [R:combobox]. | Input plus portal popup. | Value, search term, `open` state. | P1 (`defaultValue` + `modelValue`, `open` + `defaultOpen`) [src/src/runtime/components/InputMenu.vue] line 48. | Wraps Reka `ComboboxRoot` (+ TagsInput parts for multi mode) [src/src/runtime/components/InputMenu.vue]. |
| InputNumber | Numeric input with range [doc/input-number]. | Spinbutton semantics from Reka NumberField [R:number-field]. | Input with increment/decrement buttons. | Numeric model. | P1 (`defaultValue` + `modelValue` via useVModel) [src/src/runtime/components/InputNumber.vue] line 115. | Wraps `NumberFieldRoot/Input/Decrement/Increment` [src/src/runtime/components/InputNumber.vue]. |
| InputRating | Star rating collector [doc/input-rating]. | Rating semantics from Reka Rating [R:rating]. | Row of rating items. | Rating value. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/InputRating.vue] line 11. | Wraps `RatingRoot/RatingItem/RatingItemIndicator` [src/src/runtime/components/InputRating.vue]. |
| InputTags | Interactive tag input [doc/input-tags]. | Tag list editing semantics from Reka TagsInput [R:tags-input]. | Input with tag chips and delete buttons. | Tag array model. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/InputTags.vue] line 16. | Wraps `TagsInputRoot/Item/ItemText/ItemDelete/Input` [src/src/runtime/components/InputTags.vue]. |
| InputTime | Time entry input [doc/input-time]. | Segmented time-field semantics from Reka TimeField [R:time-field]. | Segmented input. | Time or range model. | P1 (`defaultValue` + `modelValue`); `range` picks TimeField vs TimeRangeField [src/src/runtime/components/InputTime.vue]. | Wraps namespaced Reka `TimeField`/`TimeRangeField` [src/src/runtime/components/InputTime.vue]. |
| Kbd | Keyboard key display [doc/kbd]. | None promised. | Single kbd element. | None. | none found. | Primitive-only [src/src/runtime/components/Kbd.vue]. |
| Link | NuxtLink wrapper with extra props [doc/link]. | Anchor semantics; `aria-current` via `active`/`exactActive` handling NOT VERIFIED. | Single anchor (or router link). | Active match evaluation. | `activeClass` vs `active` prop both influence active styling: none found as strict contradiction. | New; wraps NuxtLink, uses Reka `Slot` [src/src/runtime/components/Link.vue]. |
| Listbox | Selectable list with search and virtualization [doc/listbox]. | Listbox/option roles, typeahead, arrows from Reka Listbox [R:listbox]. | Optional filter above virtualized items. | Selection, search term. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/Listbox.vue] line 202. | Wraps `ListboxRoot/Content/Group/Virtualizer/Item/ItemIndicator/Filter` [src/src/runtime/components/Listbox.vue]. |
| LocaleSelect | Locale switcher select [doc/locale-select]. | Select semantics from USelect. | Wraps USelect with locale items. | Reads/writes locale. | none found. | Composes USelect [src/src/runtime/components/locale/LocaleSelect.vue]. |
| Main | Fills available viewport height [doc/main]. | None promised. | Single main element. | None. | none found. | Primitive-only [src/src/runtime/components/Main.vue]. |
| Marquee | Infinite scrolling content [doc/marquee]. | Motion content; reduced-motion behavior NOT VERIFIED. | Duplicated track of content. | Animation state, pause flag. | `pause` vs `pauseOnHover`: none found. | Primitive-only [src/src/runtime/components/Marquee.vue]. |
| Modal | Dialog window [doc/modal]. | `role=dialog`, `aria-modal` under modal behavior, focus trap, Esc close from Reka Dialog [R:dialog]. | Portal overlay: overlay + content + title/description/close. | `open` state. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/Modal.vue] line 116. | Wraps `DialogRoot/Trigger/Portal/Overlay/Content/Title/Description/Close` [src/src/runtime/components/Modal.vue]. |
| NavigationMenu | Horizontal or vertical link list [doc/navigation-menu]. | Disclosure and link semantics from Reka NavigationMenu [R:navigation-menu]. | List of items with optional collapsible content (accordion mode on mobile). | Active/expanded item state. | `orientation` changes layout, not value: none found. | Wraps `NavigationMenuRoot/List/Item/Trigger/Content/Link/Indicator/Viewport` + `AccordionRoot/Item/Trigger/Content` [src/src/runtime/components/NavigationMenu.vue]. |
| Page | Page grid with side columns [doc/page]. | None promised. | Left/right/main column layout. | None. | none found. | Primitive-only [src/src/runtime/components/Page.vue]. |
| PageAnchors | Anchor link list [doc/page-anchors]. | Links. | Link list. | None. | none found. | Primitive-only [src/src/runtime/components/PageAnchors.vue]. |
| PageAside | Sticky page navigation aside [doc/page-aside]. | None promised. | Sticky container. | None. | none found. | Primitive-only [src/src/runtime/components/PageAside.vue]. |
| PageBody | Main content region [doc/page-body]. | None promised. | Single container. | None. | none found. | Primitive-only [src/src/runtime/components/PageBody.vue]. |
| PageCard | Pre-styled card with optional link [doc/page-card]. | Whole-card link via Reka `Slot` composition NOT VERIFIED. | Card root. | None. | `to` vs click handler: none found. | Primitive-only [src/src/runtime/components/PageCard.vue]. |
| PageColumns | Multi-column layout [doc/page-columns]. | None promised. | Column wrapper. | None. | none found. | Primitive-only [src/src/runtime/components/PageColumns.vue]. |
| PageCTA | Call-to-action section [doc/page-cta]. | Links inside. | Section layout. | None. | none found. | Primitive-only [src/src/runtime/components/PageCTA.vue]. |
| PageFeature | Feature showcase block [doc/page-feature]. | None promised. | Icon/title/description layout. | None. | none found. | Primitive-only [src/src/runtime/components/PageFeature.vue]. |
| PageGrid | Responsive grid [doc/page-grid]. | None promised. | Grid wrapper. | None. | none found. | Primitive-only [src/src/runtime/components/PageGrid.vue]. |
| PageHeader | Page header [doc/page-header]. | Headline semantics from consumer heading levels NOT VERIFIED. | Title/description/links layout. | None. | none found. | Primitive-only [src/src/runtime/components/PageHeader.vue]. |
| PageHero | Page hero [doc/page-hero]. | Same as PageHeader. | Hero layout. | None. | none found. | Primitive-only [src/src/runtime/components/PageHero.vue]. |
| PageLinks | Link list [doc/page-links]. | Links. | Link list. | None. | none found. | Primitive-only [src/src/runtime/components/PageLinks.vue]. |
| PageList | Vertical stacked list [doc/page-list]. | None promised. | List wrapper. | None. | none found. | Primitive-only [src/src/runtime/components/PageList.vue]. |
| PageLogos | Logo/image row [doc/page-logos]. | Images with alt from items. | Flex row. | None. | none found. | Primitive-only [src/src/runtime/components/PageLogos.vue]. |
| PageSection | Responsive section [doc/page-section]. | None promised. | Section container. | None. | none found. | Primitive-only [src/src/runtime/components/PageSection.vue]. |
| Pagination | Page navigation buttons [doc/pagination]. | Button controls; current-page announcement per Reka Pagination [R:pagination]. | Nav of first/prev/items/ellipsis/next/last controls. | Current page, derived page list. | P1 (`page` + `defaultPage`) [src/src/runtime/components/Pagination.vue] line 12. | Wraps `PaginationRoot/List/Item/First/Prev/Ellipsis/Next/Last` [src/src/runtime/components/Pagination.vue]. |
| PinInput | PIN entry [doc/pin-input]. | Per-digit inputs managed as one logical field by Reka PinInput [R:pin-input]. | Row of digit inputs. | Digit array model. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/PinInput.vue] line 14. | Wraps `PinInputRoot/PinInputInput` [src/src/runtime/components/PinInput.vue]. |
| Popover | Non-modal floating dialog [doc/popover]. | Non-modal dialog pattern, Esc close, focus return from Reka Popover [R:popover]. | Trigger plus portal content. | `open` state; hover mode adds delays. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/Popover.vue] line 91. | Wraps namespaced Reka `Popover` (or `HoverCard` in hover mode) [src/src/runtime/components/Popover.vue]. |
| PricingPlan | Pricing plan card [doc/pricing-plan]. | None promised. | Card with tiers/features/button. | None. | none found. | Primitive-only [src/src/runtime/components/PricingPlan.vue]. |
| PricingPlans | Grid of pricing plans [doc/pricing-plans]. | Inherits children. | Grid of PricingPlan. | None. | none found. | Primitive-only [src/src/runtime/components/PricingPlans.vue]. |
| PricingTable | Comparison pricing table [doc/pricing-table]. | Table semantics from native table markup NOT VERIFIED. | Table/grid of tiers and features. | None. | none found. | Primitive-only [src/src/runtime/components/PricingTable.vue]. |
| Progress | Task progress indicator [doc/progress]. | `role=progressbar` with value attributes from Reka Progress [R:progress]. | Track plus indicator bar. | Value model (may be null = indeterminate). | none found. | Wraps `ProgressRoot/ProgressIndicator` [src/src/runtime/components/Progress.vue]. |
| ProgressGroup | Segmented progress bars [doc/progress-group]. | Progressbar semantics per segment. | Multiple bars sharing a total. | Segment positions array. | none found. | Wraps `ProgressRoot/ProgressIndicator` per segment [src/src/runtime/components/ProgressGroup.vue]. |
| RadioGroup | Single-choice radio set [doc/radio-group]. | `role=radiogroup`, roving tabindex, arrows from Reka RadioGroup [R:radio-group]. | Group of radio items from `items`. | Selected value. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/RadioGroup.vue]. | Wraps `RadioGroupRoot/Item/Indicator` + `Label` [src/src/runtime/components/RadioGroup.vue]. |
| ScrollArea | Scroll container with virtualization [doc/scroll-area]. | Native scroll; virtualization is visual only. | Scroll viewport. | Scroll position. | none found. | TanStack Virtual + Primitive-only [src/src/runtime/components/ScrollArea.vue], [https://tanstack.com/virtual/latest]. |
| Select | Native-style select from options [doc/select]. | Trigger + listbox popup semantics from Reka Select [R:select]. | Trigger button plus portal popup listing items. | Value, `open` state. | P1 (`open` + `defaultOpen` forwarded; value pair exists on root) [src/src/runtime/components/Select.vue] line 187. | Wraps `SelectRoot/Arrow/Trigger/Portal/Content/Viewport/Value/Label/Group/Item/ItemIndicator/ItemText/Separator` [src/src/runtime/components/Select.vue]. |
| SelectMenu | Advanced searchable select [doc/select-menu]. | Combobox input + listbox popup from Reka Combobox [R:combobox]. | Trigger/input plus portal popup. | Value, search term, `open`. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/SelectMenu.vue] line 48. | Wraps Reka `ComboboxRoot` family [src/src/runtime/components/SelectMenu.vue]. |
| Separator | Horizontal/vertical divider [doc/separator]. | `role=separator` from Reka Separator [R:separator]. | Single element. | None. | none found. | Wraps Reka `Separator` [src/src/runtime/components/Separator.vue]. |
| Sidebar | Collapsible sidebar with variants [doc/sidebar]. | Collapse toggle is a button; panel semantics NOT VERIFIED. | Sidebar shell with header/body/footer slots. | `open`/collapsed state. | none found. | Primitive-only [src/src/runtime/components/Sidebar.vue]. |
| Skeleton | Loading placeholder [doc/skeleton]. | None promised; decorative pulse. | Single div. | None. | none found. | Primitive-only [src/src/runtime/components/Skeleton.vue]. |
| Slideover | Dialog sliding from a screen edge [doc/slideover]. | Same dialog contract as Modal, from Reka Dialog [R:dialog]. | Portal overlay sliding panel. | `open` state. | P1 (`open` + `defaultOpen`) [src/src/runtime/components/Slideover.vue] line 117. | Wraps Reka `DialogRoot/Trigger/Portal/Overlay/Content/Title/Description/Close` [src/src/runtime/components/Slideover.vue]. |
| Slider | Numeric range selector [doc/slider]. | `role=slider` with value attributes, arrow keys from Reka Slider [R:slider]. | Track, range, thumb(s). | Value model (scalar or array). | none found. | Wraps `SliderRoot/Range/Track/Thumb` [src/src/runtime/components/Slider.vue]. |
| Splitter | Resizable panels with handles [doc/splitter]. | Window-splitter semantics from Reka Splitter [R:splitter]. | Panels separated by resize handles. | Panel sizes. | none found. | Wraps `SplitterGroup/Panel/ResizeHandle` [src/src/runtime/components/Splitter.vue]. |
| Stepper | Multi-step progress [doc/stepper]. | Step semantics from Reka Stepper [R:stepper]. | Items with indicator, trigger, separator, title, description. | Current step. | none found. | Wraps `StepperRoot/Item/Trigger/Indicator/Separator/Title/Description` [src/src/runtime/components/Stepper.vue]. |
| Switch | Two-state control [doc/switch]. | `role=switch`, `aria-checked`, Space/Enter toggle from Reka Switch [R:switch]. | Hidden input plus thumb control; optional label. | Checked state (`trueValue`/`falseValue`). | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/Switch.vue] line 12. | Wraps `SwitchRoot/SwitchThumb` + `Label` [src/src/runtime/components/Switch.vue]. |
| Table | Data table via TanStack Table [doc/table]. | Native `<table>` semantics; sorting/filtering controls are separate. | Native table markup generated from columns/data. | Many optional headless states: sorting, filters, selection, expansion, grouping, pagination, column order/visibility/pinning/sizing (each a v-model) [src/src/runtime/components/Table.vue] lines 332-344. | `paginationState` model vs consumer-side data slicing (two paging authorities) NOT VERIFIED as enforced conflict. | TanStack Table + Primitive-only [src/src/runtime/components/Table.vue], [https://tanstack.com/table/latest]. |
| Tabs | Tab panels [doc/tabs]. | `role=tablist/tab/tabpanel`, arrow keys, activation mode from Reka Tabs [R:tabs]. | List of triggers plus content panels. | Active tab. | P1 (`defaultValue` + `modelValue`) [src/src/runtime/components/Tabs.vue] line 41. | Wraps `TabsRoot/List/Indicator/Trigger/Content` [src/src/runtime/components/Tabs.vue]. |
| Textarea | Multi-line text input [doc/textarea]. | Native `<textarea>`; label via UFormField. | Single textarea root. | Value model. | P1 (`defaultValue` + `modelValue` via useVModel) [src/src/runtime/components/Textarea.vue] line 96. | Primitive-only [src/src/runtime/components/Textarea.vue]. |
| Theme | Headless theming scope for descendants [doc/theme]. | None (headless). | Renders only its slot. | Provides theme-default context. | none found. | New; context provided through Reka `createContext` [src/src/runtime/components/Theme.vue], [src/src/runtime/composables/useComponentProps.ts]. |
| Timeline | Event sequence with dates/icons [doc/timeline]. | None promised. | Item list with connector line. | None. | none found. | Primitive-only [src/src/runtime/components/Timeline.vue]. |
| Toast | Ephemeral feedback message [doc/toast]. | Live-region roles and swipe/timer dismissal from Reka Toast [R:toast]. | Portal toast card with title/description/actions/close. | Time remaining, open state. | none found. | Wraps `ToastRoot/Title/Description/Action/Close` [src/src/runtime/components/Toast.vue]. |
| Toaster | Toast viewport/provider [doc/toast]. | Viewport announcement behavior from Reka ToastProvider. | Provider plus viewport; must wrap app once. | Hosts toast queue via useToast. | none found. | Wraps `ToastProvider/ToastViewport/ToastPortal` [src/src/runtime/components/Toaster.vue]. |
| Tooltip | Hover/focus popup [doc/tooltip]. | Tooltip pattern from Reka Tooltip: opens on hover/focus, Esc closes [R:tooltip]. | Trigger plus portal content. | `open` state, delay timers. | P1 (`open` + `defaultOpen` forwarded) [src/src/runtime/components/Tooltip.vue] line 78. | Wraps `TooltipRoot/Trigger/Portal/Content/Arrow` [src/src/runtime/components/Tooltip.vue]. |
| Tree | Hierarchical tree view [doc/tree]. | Tree/treeitem semantics, expand/collapse, arrows from Reka Tree [R:tree]. | Nested item list, optionally virtualized. | Expansion and selection state. | P1 (`expanded` + `defaultExpanded`) [src/src/runtime/components/Tree.vue] line 34. | Wraps `TreeRoot/TreeItem/TreeVirtualizer` [src/src/runtime/components/Tree.vue]. |
| User | User name/description/avatar display [doc/user]. | None promised. | Avatar plus text stack; optionally a link. | None. | none found. | Primitive-only [src/src/runtime/components/User.vue]. |

## Components present in the repo but absent from the docs sitemap

Three `.vue` files exist in `src/runtime/components` without a docs page:

- `LinkBase.vue` — shared link base used by other components; wraps Reka
  `Primitive` [src/src/runtime/components/LinkBase.vue].
- `OverlayProvider.vue` — internal overlay host; no Reka import
  [src/src/runtime/components/OverlayProvider.vue].
- `Toaster.vue` is documented only inside the Toast page [doc/toast]; it ships
  but has no dedicated page.

Prose components (`src/runtime/components/prose/*`, 43 files) and the
`content/` + `color-mode/` subdirectories ship as typography/integration
components documented under <https://ui.nuxt.com/docs/typography> and the
integration pages. They reuse the main components and add no new Reka overlap
beyond the rows above.

## Theming

### Mechanism

Each component owns a theme file in `src/theme/*.ts` (121 files). The file
exports a Tailwind Variants configuration: `slots` (class strings per DOM
part), `variants` (classes keyed by prop values such as `color`, `size`,
`variant`), `compoundVariants`, and `defaultVariants`
([src/src/theme/button.ts]). The component template resolves its classes at
runtime: `ui.root({ class: [props.ui?.root, props.class] })`, shown for Card in
the official docs (<https://ui.nuxt.com/docs/getting-started/theme/components>).
Tailwind Variants merges conflicting classes with tailwind-merge at runtime
(same docs page, "Tailwind Variants uses tailwind-merge ... to merge classes").

Design tokens are CSS variables (`--ui-*`, e.g. `--ui-container`, semantic
colors like `bg-primary`, `text-error`) consumed by the theme class strings;
see <https://ui.nuxt.com/docs/getting-started/theme/css-variables>. The module
generates color variants dynamically from `theme.colors` option, visible in the
spread `Object.fromEntries((options.theme.colors || []).map(...))` in
[src/src/theme/button.ts].

Consumers override styles at three levels, all merged at runtime:

1. Global: `app.config.ts` key `ui.<component>` with the same slots/variants
   shape (<https://ui.nuxt.com/docs/getting-started/theme/components>,
   section "Global config").
2. Scoped: the `<UTheme>` component provides per-subtree defaults via
   `ThemeContext` ([src/src/runtime/components/Theme.vue]).
3. Instance: the `ui` prop (per-slot classes) and `class` prop (root/base
   slot).

Resolution order and merge behavior sit in
[src/src/runtime/composables/useComponentProps.ts]: explicit prop > nearest
`UTheme` > `app.config.ui.<name>.defaultVariants` > `withDefaults`; the `ui`
and `class` props are merged with `defu` instead of replaced, and the explicit
class wins tailwind-merge's last-in-wins resolution.

### Worked example: change one part of one Button

Goal: make the trailing icon rotate on one button, and make all buttons bold
globally.

Instance level (one component, one slot):

```vue
<UButton trailing-icon="i-lucide-chevron-right" size="md" :ui="{
  trailingIcon: 'rotate-90 size-3'
}">
```

The docs show exactly this example and note that `size-3` overrides the `size-5`
that the `md` size variant would apply
(<https://ui.nuxt.com/docs/getting-started/theme/components>, section "`ui`
prop"). The slot key `trailingIcon` matches the slot declared in
[src/src/theme/button.ts].

Global level (every Button, one slot):

```ts
// app/app.config.ts
export default defineAppConfig({
  ui: {
    button: {
      slots: { base: 'font-bold' }
    }
  }
})
```

The docs show this example and explain that `font-bold` overrides the theme's
`font-medium` on all buttons because tailwind-merge drops the earlier
conflicting class (<https://ui.nuxt.com/docs/getting-started/theme/components>,
section "Global config", note after the example). At runtime the component
reads `appConfig.ui.button` through `useComponentProps('button', props)`, which
defu-merges the consumer's `ui` object over the theme defaults before `tv()`
resolves the final class strings
([src/src/runtime/composables/useComponentProps.ts]).

A slot can also be replaced instead of merged by passing a function:
`ui="{ label: () => 'text-base font-bold' }"` receives the resolved default
classes as its argument (same docs page, "`ui` prop" notes).

### What this mechanism is

This whole pipeline — string class slots resolved by `tv()` at runtime,
consumer objects deep-merged by `defu`, conflicts settled by tailwind-merge
after render — is exactly the runtime/class-merging mechanism that a
compile-time-checked component library would replace.
