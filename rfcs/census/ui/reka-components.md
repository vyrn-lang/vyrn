# Source 1: reka-ui component census

Census of the components `reka-ui` ships. The census records what each component
promises and which prop pairs can hold contradictory values at the same time. It
designs nothing.

## Method

The component list comes from the official docs index
(<https://www.reka-ui.com/llms.txt>). Every fact below cites the component's docs
page under <https://www.reka-ui.com/docs/components/> or
<https://www.reka-ui.com/docs/utilities/>. Each row cites its page once, in the
accessibility column; every cell in that row draws on that same page (anatomy,
API reference tables, accessibility section). Prop types quoted in this file are
copied from those pages on 2026-08-23. The GitHub API returned HTTP 403 during
collection, so source-file claims carry no repo citation; all claims rest on the
docs pages.

Count: 64 components (16 form, 5 color, 12 date, 24 general, 7 utility). The
docs also list 8 composables (`useId`, `useDateFormatter`, `useDirection`,
`useLocale`, `useEmitAsProps`, `useFilter`, `useForwardExpose`,
`useForwardProps`, `useForwardPropsEmits`); composables are not components and
get no rows.

## Contradictory-pair codes

The last column uses these codes. Every code is verified against a props table
on a cited page; the summary section counts them.

- P1 - Controlled plus initial-value props both settable on one part
  (`modelValue`+`defaultValue`, `open`+`defaultOpen`, `page`+`defaultPage`,
  `snapPoint`+`defaultSnapPoint`, `expanded`+`defaultExpanded`). The controlled
  state guide (<https://www.reka-ui.com/docs/guides/controlled-state>) tells the
  developer to pick one mode. The types allow both at once, and no page states
  which value wins.
- P2 - `disabled` + `required`. A disabled form control takes no input and does
  not submit a name/value pair, so `required` can never be satisfied through
  user input.
- P3 - `multiple: boolean` decoupled from the value type. The value type stays a
  union such as `T | T[]` or `DateValue | DateValue[] | null` whether or not
  `multiple` is set, so an array value with `multiple: false` type-checks.
- P4 - `type: "single" | "multiple"` with the value type unchanged
  (`modelValue: T` / `AcceptableValue | AcceptableValue[]` regardless of
  `type`).
- P5 - Root `modal: false` + Content `disableOutsidePointerEvents: true`.
  Non-modal state promises interaction with the outside page; disabling outside
  pointer events removes exactly that.
- P6 - Cross-constrained numbers freely settable: `min` above `max`,
  `modelValue` above `max`, `itemsPerPage: 0` with nonzero `total`,
  `minSize` above `maxSize`.
- P7 - Two props share one value space with no exclusion: equal channels
  (`xChannel === yChannel`), a channel outside the set `colorSpace`, or
  `trueValue === falseValue`.
- P8 - One prop owned at two levels with no stated precedence (Provider vs
  Root, Group vs Item).
- P9 - Two value states for one concept inside one component tree (selection
  value vs search text).
- P10 - A pair of settings whose simultaneous values leave no way to enter a
  state.
- P11 - `rovingFocus: false` + `loop: true`. Loop describes arrow-key wraparound;
  arrows do nothing without roving focus.
- P12 - A value prop that names another part's id or value, with nothing
  checking that the target exists or is enabled.
- P13 - `decorative: true` cancels `orientation`: the orientation leaves the
  accessibility tree while the prop still renders.

## Component table

|component|what it is|the accessibility contract it promises|the DOM structure it requires|the state it owns|the props that can be set to a contradictory pair|
|---|---|---|---|---|---|
|Accordion|Vertically stacked headings that reveal content sections.|Follows the Accordion WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/accordion)).|Root > Item > Header > Trigger, Content as Item child. Item requires a unique `value`.|Open item(s) via `type: "single" \| "multiple"`, `collapsible`, per-item disabled.|P1 (`modelValue`+`defaultValue`, typed plain `T` whatever `type` says); P4 (`type="multiple"` with single-value `T`); P8 (`disabled` on Root and again on Item); P12 (`defaultValue` can name a missing or disabled Item value).|
|Alert Dialog|Modal dialog that interrupts the user and expects a response.|Follows the Alert Dialog WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/alert-dialog)).|Root > Trigger > Portal > Overlay + Content; Title required inside Content; Cancel and Action close it.|Open/closed (`open`+`defaultOpen`). No `modal` prop; always modal.|P1 (`open`+`defaultOpen`).|
|Aspect Ratio|Box that keeps a fixed width/height ratio.|None documented ([docs](https://www.reka-ui.com/docs/components/aspect-ratio)).|Single wrapper element; content must be its child.|None (a `ratio` number).|none found.|
|Autocomplete|Text input with suggestions; the value is the typed text, not an item.|Follows the Combobox WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/autocomplete)).|Root > Anchor wrapping Input, Trigger, Cancel; Portal > Content > Viewport holding Empty, Items, Groups; Arrow optional.|Open state, selected/typed value (`modelValue: string`), search text on Input, item collection registration, highlighted item.|P1 (`open`+`defaultOpen`, `modelValue`+`defaultValue`); P2 (`disabled`+`required` on Root); P9 (Root `modelValue` selection vs Input `modelValue` search text).|
|Avatar|Image with fallback for user representation.|None documented ([docs](https://www.reka-ui.com/docs/components/avatar)).|Root > Image + Fallback; Fallback shows until Image loads or errors.|Internal image load status.|none found.|
|Calendar|Month grid for picking dates.|Docs md export has an Accessibility section heading with empty body; keyboard behaviour NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/calendar)).|Header (Prev, Heading, Next) then Grid > GridHead/GridBody > Row > Cell > CellTrigger.|Placeholder date, view paging (`numberOfMonths`, `pagedNavigation`), value `DateValue \| DateValue[] \| null`, `multiple`.|P1 (`modelValue`+`defaultValue`, `placeholder`+`defaultPlaceholder`); P3 (`multiple` vs union value type); P6 (`minValue` above `maxValue`).|
|Checkbox|Control toggling checked, unchecked, indeterminate.|Follows the tri-state Checkbox WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/checkbox)).|Root > Indicator; optional GroupRoot wrapping several Roots; hidden `input` renders inside a form.|Checked state `null \| T \| "indeterminate"`; group array value.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required` on Root and on GroupRoot); P7 (`trueValue` and `falseValue` share type `T`; both can be set to the same value).|
|Collapsible|Single panel that expands and collapses.|Follows the Disclosure WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/collapsible)).|Root > Trigger + Content; Trigger must precede Content.|Open/closed.|P1 (`open`+`defaultOpen`).|
|Color Area|Two-dimensional picker for two channels of a color space.|Keyboard contract: arrows adjust x/y channel by one step, Shift steps by 10, PageUp/PageDown large y steps, Home/End jump x ([docs](https://www.reka-ui.com/docs/components/color-area)).|Root > Area > Thumb; Thumb positioned by slot style binding.|Color value `string \| Color`, `colorSpace`, `xChannel`, `yChannel`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P7 (`xChannel` may equal `yChannel`; either channel may name a channel outside `colorSpace`, since the channel union spans hsl, rgb, hsb).|
|Color Field|Text input for editing one color-channel value.|Keyboard contract: arrows step, PageUp/PageDown ten steps, Home/End clamp, Enter commits ([docs](https://www.reka-ui.com/docs/components/color-field)).|Root > Input.|Color value, active `channel`, `step`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P7 (`channel` union spans all spaces while `colorSpace` is separate).|
|Color Slider|Slider adjusting one color channel.|Keyboard contract: arrows step in the matching orientation, PageUp/PageDown large steps, Home/End clamp ([docs](https://www.reka-ui.com/docs/components/color-slider)).|Root > Track > Thumb.|Color value, `channel`, `orientation`, `inverted`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P7 (`channel` vs `colorSpace` mismatch possible).|
|Color Swatch|Static block showing one color.|None documented ([docs](https://www.reka-ui.com/docs/components/color-swatch)).|Single element taking a `color`.|None.|none found.|
|Color Swatch Picker|Picker selecting from predefined swatches.|No accessibility section on the docs page ([docs](https://www.reka-ui.com/docs/components/color-swatch-picker)).|Root > Item > Swatch + Indicator; each Item needs a `value`.|Selection `string \| string[]`, `multiple`, `selectionBehavior: "replace" \| "toggle"`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P3 (`multiple` vs union value type; `selectionBehavior="toggle"` has single-select meaning only under `multiple`).|
|Combobox|List-backed choice with free-text filtering and full keyboard support.|Follows the Combobox WAI-ARIA pattern and its Autocomplete List example ([docs](https://www.reka-ui.com/docs/components/combobox)).|Root > Anchor (Input, Trigger, Cancel); Portal > Content > Viewport > Items/Groups; ItemIndicator optional.|Open state, selection `T \| T[]`, `multiple`, search text, filter, highlighted item.|P1 (`open`+`defaultOpen`, `modelValue`+`defaultValue`); P2 (`disabled`+`required`); P3 (`multiple` vs union value type); P9 (Root selection vs Input `modelValue` search text).|
|Config Provider|Wrapper supplying global configuration (direction, locale).|Not applicable ([docs](https://www.reka-ui.com/docs/utilities/config-provider)).|App-wide wrapper element.|Provided config values.|none found.|
|Context Menu|Menu at the pointer, opened by right-click or long-press.|Uses roving tabindex for focus movement among items ([docs](https://www.reka-ui.com/docs/components/context-menu)).|Root > Trigger > Portal > Content > Items, Groups, CheckboxItems, RadioGroup > RadioItems, Sub > SubTrigger > SubContent; Checkbox/Radio items need Indicator to show state.|Root open state is internal only (no `open` prop); RadioGroup value; per-CheckboxItem checked state; Sub open state.|Sub-level P1 (`open`+`defaultOpen`); otherwise none found on Root (no controlled open exists).|
|Date Field|Segmented input for one date. Page marked Alpha.|Accessibility section present but empty in the md export; keyboard details NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/date-field)).|Root > Input (segments rendered by the Input).|Date value, placeholder segment, granularity.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P6 (`minValue` above `maxValue`).|
|Date Picker|Date field plus calendar popover.|Accessibility section empty in the md export; keyboard details NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/date-picker)).|Field wrapping Input and Trigger; Anchor; Content holding Close, Arrow, Calendar parts.|Open state, date value, granularity, `closeOnSelect`.|P1 (`open`+`defaultOpen`, `modelValue`+`defaultValue`); P2 (`disabled` on Root).|
|Date Range Field|Segmented input for two dates. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/date-range-field)).|Root > Input segments.|Range value `DateRange`, placeholder.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Date Range Picker|Range field plus calendar popover.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/date-range-picker)).|Like Date Picker around range field and calendar parts.|Open state, `DateRange` value, `fixedDate: "start" \| "end"`, `allowNonContiguousRanges`.|P1 (`open`+`defaultOpen`, `modelValue`+`defaultValue`); P2 (`disabled` on Root).|
|Dialog|Window over the page that makes content underneath inert.|Follows the Dialog WAI-ARIA pattern; docs instruct labelling icon-only Close buttons ([docs](https://www.reka-ui.com/docs/components/dialog)).|Root > Trigger > Portal > Overlay + Content; Title expected inside Content; Description and Close optional.|Open/closed, `modal`.|P1 (`open`+`defaultOpen`); P5 (`modal: false` + Content `disableOutsidePointerEvents: true`).|
|Drawer|Panel sliding from a screen edge with swipe dismissal and snap points.|Page cites the Dialog WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/drawer)).|Like Dialog plus Handle inside Content; SwipeArea, Viewport, Indent parts exist.|Open state, snap points, `modal: false \| true \| "trap-focus"`, swipe direction.|P1 (`open`+`defaultOpen`, `snapPoint`+`defaultSnapPoint`); P5 (`modal: false` + Content `disableOutsidePointerEvents: true`).|
|Dropdown Menu|Button-triggered menu of actions.|Follows the Menu Button WAI-ARIA pattern; roving tabindex among items ([docs](https://www.reka-ui.com/docs/components/dropdown-menu)).|Root > Trigger > Portal > Content > Label, Items, Groups, CheckboxItem+Indicator, RadioGroup > RadioItem, Sub tree, Separator, Arrow.|Open state, `modal`, RadioGroup value, per-CheckboxItem checked state, Sub open state.|P1 (`open`+`defaultOpen` on Root; again on Sub).|
|Editable|Inline text shown static, edited through triggers. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/editable)).|Root > Area (Preview + Input) plus EditTrigger, SubmitTrigger, CancelTrigger.|Edit-mode flag, value `string \| null`, `submitMode`, `activationMode`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P10 (`activationMode="none"` + `startWithEditMode: false` leaves no way to enter edit mode).|
|Focus Scope|Traps and loops keyboard focus inside a boundary.|None documented ([docs](https://www.reka-ui.com/docs/utilities/focus-scope)).|Wrapper element around focusable children.|`trapped`, `loop`, `present`.|none found.|
|Hover Card|Preview shown near a link on hover.|Docs state the content is inaccessible to keyboard users by design ([docs](https://www.reka-ui.com/docs/components/hover-card)).|Root > Trigger > Portal > Content (+Arrow).|Open state, open/close delays.|P1 (`open`+`defaultOpen`).|
|Label|Accessible label tied to a control.|Based on the native `label` element; associates by wrapping or by `for`; custom controls must build on native elements ([docs](https://www.reka-ui.com/docs/components/label)).|Single `label` element.|None.|none found.|
|Listbox|Selectable list supporting single and multiple choice.|Follows the Listbox WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/listbox)).|Root > Filter + Content > Items, Groups, Virtualizer; Items need unique `value`s.|Selection `AcceptableValue \| AcceptableValue[]`, `multiple`, filter text, `selectionBehavior`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P3 (`multiple` vs union value type); P9 (Root selection vs Filter `modelValue`).|
|Menubar|Persistent desktop-style menu bar.|Page cites the Menu Button WAI-ARIA pattern; roving tabindex among items ([docs](https://www.reka-ui.com/docs/components/menubar)).|Root > Menu > Trigger > Portal > Content menu tree; each Menu carries a `value` id.|Open-menu id (`modelValue: string`), RadioGroup and CheckboxItem states, Sub open state.|P1 (`modelValue`+`defaultValue` on Root; Sub too); P12 (`modelValue` can name a Menu id no MenubarMenu defines).|
|Month Picker|Grid for picking one month.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/month-picker)).|Header (Prev, Heading, Next) then Grid > GridBody > Row > Cell > CellTrigger.|Month value `DateValue \| DateValue[] \| null`, `multiple`, placeholder, min/max months.|P1 (`modelValue`+`defaultValue`); P3 (`multiple` vs union value type); P6 (`minValue` above `maxValue`).|
|Month Range Picker|Grid for picking a month range.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/month-range-picker)).|Same grid shape as Month Picker.|`DateRange` value, `fixedDate`, `maximumMonths`.|P1 (`modelValue`+`defaultValue`); P6 (`minValue` above `maxValue`).|
|Navigation Menu|Collection of links with hover/click-revealed panels.|Adheres to the `navigation` role requirements instead of the menu pattern, citing w3c/aria-practices issue 353 ([docs](https://www.reka-ui.com/docs/components/navigation-menu)).|Root > List > Item (Trigger + Content with Links); Viewport sits outside List; Sub variant nests its own List and value.|Open item id, hover/click trigger flags, delays, orientation.|P1 (`modelValue`+`defaultValue` on Root and again on Sub); P10 (`disableClickTrigger: true` + `disableHoverTrigger: true` leaves no way to open content).|
|Number Field|Numeric entry with increment/decrement steppers.|Follows the Spinbutton WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/number-field)).|Root > Decrement, Input, Increment; Decimal and Percentage slots for formatted display.|Value `number \| null`, `min`, `max`, `step`, format options, locale.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P6 (`min` above `max`); P8 (`disabled` owned by Root and again by Increment/Decrement).|
|Pagination|Paged navigation controls. Renders a `nav` element by default.|No accessibility section on the docs page ([docs](https://www.reka-ui.com/docs/components/pagination)).|Root > List > First, Prev, ListItem(s), Ellipsis, Next, Last.|Current page, `total`, `itemsPerPage`, `siblingCount`.|P1 (`page`+`defaultPage`); P6 (`itemsPerPage: 0` with nonzero `total`).|
|Pin Input|Sequence of one-character inputs for codes. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/pin-input)).|Root > one or more Inputs.|Character array typed by `type`, `mask`, `otp` mode.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Popover|Rich content in a portal, triggered by a button.|Page cites the Dialog WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/popover)).|Root > Trigger + Anchor; Portal > Content > Close, Arrow.|Open state, `modal`.|P1 (`open`+`defaultOpen`); P5 (`modal: false` + Content `disableOutsidePointerEvents: true`).|
|Presence|Mounts/unmounts a child only after its transition finishes.|Not applicable ([docs](https://www.reka-ui.com/docs/utilities/presence)).|Wraps one child.|`present` boolean.|none found.|
|Primitive|Renders any element or component with Reka behaviour merged.|Not applicable ([docs](https://www.reka-ui.com/docs/utilities/primitive)).|Whatever `as` names; `asChild` merges onto the child.|None beyond passthrough props.|none found.|
|Progress|Completion indicator.|Adheres to the `progressbar` role requirements; page links the meter pattern ([docs](https://www.reka-ui.com/docs/components/progress)).|Root > Indicator.|Value `number \| null`, `max`.|P6 (`modelValue` above `max`; `max` at or below 0).|
|Radio Group|Set of radio buttons with one allowed check.|Follows the Radio Group WAI-ARIA pattern; roving tabindex among items ([docs](https://www.reka-ui.com/docs/components/radio-group)).|Root > Item > Indicator; each Item needs a unique `value`.|One selected value; roving-focus current id; `loop`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P8 (`name` and `required` owned by Root and again by Item); P12 (`defaultValue` can name an absent or disabled Item value).|
|Range Calendar|Month grid restricted to date ranges.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/range-calendar)).|Same grid shape as Calendar.|`DateRange` value, `maximumDays`, `fixedDate`, `allowNonContiguousRanges`, placeholder.|P1 (`modelValue`+`defaultValue`); P6 (`minValue` above `maxValue`, `maximumDays` at or below 0).|
|Rating|Star score input with fractional steps. Page marked Alpha.|Built on Radio Group and follows that pattern; docs require an `aria-label` per step ([docs](https://www.reka-ui.com/docs/components/rating)).|Root (slot exposes items) > Item > ItemIndicator; each Item carries its numeric `item`.|Score `number`, hover state, `clearable`, `length`, `step`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P6 (`length` at or below 0).|
|Roving Focus|Implements roving tabindex across items.|Follows the composite-component keyboard-navigation practice ([docs](https://www.reka-ui.com/docs/utilities/roving-focus)).|Group wrapping Items.|`currentTabStopId`/`defaultCurrentTabStopId`, orientation, `loop`.|P12 (`currentTabStopId`/`defaultCurrentTabStopId` can name an absent Item id).|
|Scroll Area|Custom-styled scrollbars keeping native scrolling and keyboard scrolling.|Docs advise native scrolling first; keyboard scrolling preserved because scrolling stays native ([docs](https://www.reka-ui.com/docs/components/scroll-area)).|Root > Viewport; Scrollbar(orientation) > Thumb; Corner. Viewport must be the scroll container.|Scrollbar visibility `type`, hide delay, per-scrollbar orientation.|none found.|
|Select|Button-triggered option list.|Follows the Listbox pattern plus the Select-Only Combobox example; docs point to Label for an accessible name ([docs](https://www.reka-ui.com/docs/components/select)).|Trigger (Value, Icon) > Portal > Content > Viewport > Item (ItemText, ItemIndicator), Groups, scroll buttons, Arrow.|Open state, value `T \| T[]`, `multiple`, nullable value.|P1 (`open`+`defaultOpen`, `modelValue`+`defaultValue`); P2 (`disabled`+`required`); P3 (`multiple` vs union value type).|
|Separator|Visual or semantic divider.|Adheres to the `separator` role requirements ([docs](https://www.reka-ui.com/docs/components/separator)).|Single element.|`orientation`, `decorative`.|P13 (`decorative: true` keeps rendering `orientation` while removing it from the accessibility tree).|
|Year Picker|Grid for picking one year.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/year-picker)).|Header (Prev, Heading, Next) then Grid > GridBody > Row > Cell > CellTrigger.|Year value `DateValue \| DateValue[] \| null`, `multiple`, `yearsPerPage`, placeholder, min/max years.|P1 (`modelValue`+`defaultValue`); P3 (`multiple` vs union value type); P6 (`minValue` above `maxValue`).|
|Year Range Picker|Grid for picking a year range.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/year-range-picker)).|Same grid shape as Year Picker.|`DateRange` value, `fixedDate`, `maximumYears`, `yearsPerPage`.|P1 (`modelValue`+`defaultValue`); P6 (`minValue` above `maxValue`).|
|Slider|Track with thumbs selecting one or more values.|Follows the Slider (two-thumb) pattern; docs document how `inverted` flips specific keys depending on `orientation` ([docs](https://www.reka-ui.com/docs/components/slider)).|Root > Track > Range; one or more Thumbs; thumb count equals array length.|Values `number[]`, `min`, `max`, `step`, `inverted`, `minStepsBetweenThumbs`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P6 (`min` above `max`).|
|Splitter|Layout divided into resizable panels.|Follows the Window Splitter pattern ([docs](https://www.reka-ui.com/docs/components/splitter)).|Group > Panels with ResizeHandle between; handle adjacency decides resize relations.|Panel sizes, collapsed sizes, optional saved layout via `autoSaveId`.|P6 (`minSize` above `maxSize`, `defaultSize` outside the range).|
|Stepper|Step indicators for a multi-step process. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/stepper)).|Root > Item > Trigger, Indicator, Title, Description, Separator; Items carry explicit `step` numbers.|Current step number, `linear`, per-item completed/disabled flags.|P1 (`modelValue`+`defaultValue`); P12 (duplicate `step` numbers across Items go undetected; `defaultValue` can name an absent step).|
|Switch|Binary toggle control.|Adheres to the `switch` role requirements ([docs](https://www.reka-ui.com/docs/components/switch)).|Root > Thumb; hidden `input` inside a form.|On/off state typed `null \| T` with `trueValue`/`falseValue`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Tabs|Tab list with panels shown one at a time.|Follows the Tabs WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/tabs)).|Root > List (Trigger per tab, Indicator) + Content per tab; Trigger and Content match by `value`.|Active tab value, `activationMode: "automatic" \| "manual"`, per-trigger disabled.|P1 (`modelValue`+`defaultValue`); P12 (`defaultValue` can name an absent or disabled Trigger value).|
|Tags Input|Tags rendered inside an input followed by a text input. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/tags-input)).|Root > Item (Text, Delete), Input, Clear.|Tag array `T[]`, `max`, delimiter, duplicate handling, convert/display functions.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Time Field|Segmented time input. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/time-field)).|Root > Input segments.|Time value, placeholder, granularity.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Time Range Field|Segmented start/end time input. Page marked Alpha.|Accessibility section empty in the md export; NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/time-range-field)).|Root > Input segments with `part` and `type` props assigning each segment to a boundary.|Time range value, placeholder.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Toast|Temporary messages announced to assistive technology.|Adheres to `aria-live` requirements; `type` picks foreground (immediate) or background announcement ([docs](https://www.reka-ui.com/docs/components/toast)).|Provider > Viewport; Root (Title, Description, Action, Close) inside Provider; Action requires `altText`.|Per-toast open state, durations, swipe settings, viewport hotkey.|P1 (`open`+`defaultOpen` on Root); P8 (`duration` owned by Provider and again by Root).|
|Toggle|Two-state button.|Accessibility section empty in the md export; pressed-state semantics NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/toggle)).|Single button element.|Pressed state `boolean \| null`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`).|
|Toggle Group|Set of two-state buttons.|Uses roving tabindex among items ([docs](https://www.reka-ui.com/docs/components/toggle-group)).|Root > Items carrying `value`s.|Single or multiple pressed values via `type`, `rovingFocus`, `loop`.|P1 (`modelValue`+`defaultValue`); P2 (`disabled`+`required`); P4 (`type="single"/"multiple"` with value type unchanged `AcceptableValue \| AcceptableValue[]`); P11 (`rovingFocus: false` + `loop: true`).|
|Toolbar|Grouped toolbar controls.|Uses roving tabindex among items ([docs](https://www.reka-ui.com/docs/components/toolbar)).|Root > Button, Link, ToggleGroup > ToggleItem, Separator.|Roving-focus position; embedded ToggleGroup single/multiple values.|P4 (`type` vs unchanged value type on ToolbarToggleGroup); P11 (`rovingFocus: false` + `loop: true` on ToolbarToggleGroup); P2 (`disabled`+`required` on ToolbarToggleGroup).|
|Tooltip|Popup on hover or keyboard focus.|Accessibility section empty in the md export; tooltip role semantics NOT VERIFIED ([docs](https://www.reka-ui.com/docs/components/tooltip)).|Provider > Root > Trigger > Portal > Content (+Arrow).|Open state, delay duration, provider skip-delay bookkeeping.|P1 (`open`+`defaultOpen`); P8 (`delayDuration`, `disableClosingTrigger`, `disableHoverableContent`, `ignoreNonKeyboardFocus`, `disabled` each owned by Provider and again by Root).|
|Tree|Hierarchical expandable item view. Page marked Alpha.|Follows the Tree View WAI-ARIA pattern ([docs](https://www.reka-ui.com/docs/components/tree)).|Root > Items (or Virtualizer > Items); hierarchy derives from `items` plus `getKey`/`getChildren`.|Expanded ids `string[]`, selection typed `(M extends true ? U[] : U)` against `multiple: boolean \| M`, selection behavior.|P1 (`expanded`+`defaultExpanded`, `modelValue`+`defaultValue`); P3 (`multiple: boolean \| M` decoupled from the conditional value type).|
|Visually Hidden|Hides content visually while keeping it accessible.|Docs position it as an alternative to `aria-label` or `aria-labelledby` labelling ([docs](https://www.reka-ui.com/docs/utilities/visually-hidden)).|Wrapper span with clip styles.|None.|none found.|
|Slot|Merges its props onto its immediate child.|No accessibility claims on the docs page; the page documents prop merging only ([docs](https://www.reka-ui.com/docs/utilities/slot)).|No element of its own after the merge.|None.|none found.|

## Contradictory pair summary

Distinct patterns and the number of components exhibiting each:

|pattern|shape|components|
|---|---|---|
|P1|controlled + initial-value props both settable, precedence unstated|51|
|P2|`disabled` + `required`|24|
|P3|`multiple` boolean decoupled from value shape|9|
|P4|`type: "single" \| "multiple"` with value type unchanged|3|
|P5|`modal: false` + Content `disableOutsidePointerEvents: true`|3|
|P6|cross-constrained numbers freely settable|7|
|P7|shared value space with no exclusion|4|
|P8|one prop owned at two levels|5|
|P9|two value states for one concept|3|
|P11|`rovingFocus: false` + `loop: true`, where `loop` describes arrow-key wraparound arrows can no longer do|3|
|P12|unvalidated cross-part id/value reference|6|
|P13|`decorative` cancels `orientation`|1|

P1 count covers components where a controlled prop and its initial-value
counterpart appear on the same part, including nested repeats counted once
(Sub, Group, Provider levels). P12 counts Accordion, Menubar, Radio Group,
Roving Focus, Stepper, Tabs; six components, listed here individually.

## Verification gaps

- Pages whose md export contains an Accessibility heading with an empty body:
  Calendar, Date Field, Date Picker, Date Range Field, Date Range Picker,
  Editable, Month Picker, Month Range Picker, Pin Input, Range Calendar,
  Stepper, Tags Input, Time Field, Time Range Field, Toggle, Tooltip, Year
  Picker, Year Range Picker. Keyboard contracts for these are NOT VERIFIED.
- Pages with no Accessibility section at all: Aspect Ratio, Avatar, Color Swatch
  Picker, Color Swatch, Config Provider, Focus Scope, Pagination, Presence,
  Primitive, Slot. Their accessibility contracts read "none documented".
- Slot ships as a utility with no anatomy and no API table on its page; its row
  records only what the page states.
