// Offline Slack emoji shortcodes → unicode, for message text and reaction chips.
// A static table of the shortcodes people actually use in work threads (Slack's
// own names, incl. its aliases); anything else — custom workspace emoji included —
// stays as `:name:` (custom emoji need `emoji:read`, out of scope).
// Pure module (no imports): unit-tested with `node --test`.

const EMOJI: Record<string, string> = {
  // Hands & gestures
  '+1': '👍', thumbsup: '👍', '-1': '👎', thumbsdown: '👎', ok_hand: '👌', clap: '👏',
  pray: '🙏', raised_hands: '🙌', wave: '👋', muscle: '💪', point_up: '☝️',
  point_right: '👉', point_left: '👈', point_down: '👇', point_up_2: '👆',
  v: '✌️', crossed_fingers: '🤞', handshake: '🤝', raised_hand: '✋', hand: '✋',
  open_hands: '👐', writing_hand: '✍️', fist: '✊', facepunch: '👊', punch: '👊',
  call_me_hand: '🤙', metal: '🤘', palms_up_together: '🤲', eyes: '👀', eye: '👁️',
  brain: '🧠', bow: '🙇', man_bowing: '🙇‍♂️', woman_bowing: '🙇‍♀️',
  // Faces
  smile: '😄', smiley: '😃', grinning: '😀', grin: '😁', laughing: '😆', satisfied: '😆',
  sweat_smile: '😅', joy: '😂', rolling_on_the_floor_laughing: '🤣', rofl: '🤣',
  slightly_smiling_face: '🙂', upside_down_face: '🙃', wink: '😉', blush: '😊',
  innocent: '😇', heart_eyes: '😍', star_struck: '🤩', kissing_heart: '😘',
  yum: '😋', stuck_out_tongue: '😛', stuck_out_tongue_winking_eye: '😜', zany_face: '🤪',
  thinking_face: '🤔', thinking: '🤔', face_with_hand_over_mouth: '🤭', shushing_face: '🤫',
  neutral_face: '😐', expressionless: '😑', no_mouth: '😶', smirk: '😏', unamused: '😒',
  face_with_rolling_eyes: '🙄', grimacing: '😬', relieved: '😌', pensive: '😔',
  sleepy: '😪', sleeping: '😴', mask: '😷', nerd_face: '🤓', sunglasses: '😎',
  confused: '😕', worried: '😟', slightly_frowning_face: '🙁', white_frowning_face: '☹️',
  open_mouth: '😮', hushed: '😯', astonished: '😲', flushed: '😳', pleading_face: '🥺',
  cry: '😢', sob: '😭', scream: '😱', confounded: '😖', persevere: '😣',
  disappointed: '😞', sweat: '😓', weary: '😩', tired_face: '😫', yawning_face: '🥱',
  triumph: '😤', rage: '😡', angry: '😠', exploding_head: '🤯', partying_face: '🥳',
  hugging_face: '🤗', hugs: '🤗', saluting_face: '🫡', melting_face: '🫠',
  face_palm: '🤦', facepalm: '🤦', shrug: '🤷', skull: '💀', ghost: '👻', robot_face: '🤖',
  see_no_evil: '🙈', hear_no_evil: '🙉', speak_no_evil: '🙊', smiling_imp: '😈',
  money_mouth_face: '🤑', zipper_mouth_face: '🤐', dizzy_face: '😵', cold_sweat: '😰',
  // Hearts & symbols
  heart: '❤️', orange_heart: '🧡', yellow_heart: '💛', green_heart: '💚', blue_heart: '💙',
  purple_heart: '💜', black_heart: '🖤', white_heart: '🤍', broken_heart: '💔',
  sparkling_heart: '💖', two_hearts: '💕', '100': '💯', fire: '🔥', sparkles: '✨',
  star: '⭐', star2: '🌟', boom: '💥', collision: '💥', zap: '⚡', tada: '🎉',
  confetti_ball: '🎊', balloon: '🎈', gift: '🎁', trophy: '🏆', medal: '🏅',
  first_place_medal: '🥇', rocket: '🚀', bulb: '💡', moneybag: '💰', gem: '💎',
  bell: '🔔', no_bell: '🔕', mega: '📣', loudspeaker: '📢', speech_balloon: '💬',
  thought_balloon: '💭', zzz: '💤', dart: '🎯', crown: '👑', key: '🔑', lock: '🔒',
  unlock: '🔓', link: '🔗', pushpin: '📌', round_pushpin: '📍', paperclip: '📎',
  memo: '📝', pencil: '📝', pencil2: '✏️', book: '📖', books: '📚', bookmark: '🔖',
  calendar: '📆', date: '📅', clipboard: '📋', chart_with_upwards_trend: '📈',
  chart_with_downwards_trend: '📉', bar_chart: '📊', email: '📧', envelope: '✉️',
  inbox_tray: '📥', outbox_tray: '📤', package: '📦', file_folder: '📁',
  open_file_folder: '📂', computer: '💻', desktop_computer: '🖥️', keyboard: '⌨️',
  iphone: '📱', phone: '☎️', telephone_receiver: '📞', camera: '📷', mag: '🔍',
  mag_right: '🔎', hammer: '🔨', wrench: '🔧', hammer_and_wrench: '🛠️', gear: '⚙️',
  nut_and_bolt: '🔩', bug: '🐛', beetle: '🪲', ant: '🐜', construction: '🚧',
  rotating_light: '🚨', hourglass: '⌛', hourglass_flowing_sand: '⏳', stopwatch: '⏱️',
  alarm_clock: '⏰', watch: '⌚', coffee: '☕', tea: '🍵', beer: '🍺', beers: '🍻',
  pizza: '🍕', cake: '🍰', birthday: '🎂', sushi: '🍣', ramen: '🍜', rice: '🍚',
  // Marks
  white_check_mark: '✅', heavy_check_mark: '✔️', ballot_box_with_check: '☑️',
  x: '❌', negative_squared_cross_mark: '❎', heavy_multiplication_x: '✖️',
  warning: '⚠️', no_entry: '⛔', no_entry_sign: '🚫', exclamation: '❗',
  heavy_exclamation_mark: '❗', grey_exclamation: '❕', question: '❓',
  grey_question: '❔', bangbang: '‼️', interrobang: '⁉️', heavy_plus_sign: '➕',
  heavy_minus_sign: '➖', arrow_right: '➡️', arrow_left: '⬅️', arrow_up: '⬆️',
  arrow_down: '⬇️', arrows_counterclockwise: '🔄', repeat: '🔁', information_source: 'ℹ️',
  new: '🆕', ok: '🆗', up: '🆙', cool: '🆒', free: '🆓', sos: '🆘', ng: '🆖',
  red_circle: '🔴', large_orange_circle: '🟠', large_yellow_circle: '🟡',
  large_green_circle: '🟢', large_blue_circle: '🔵', large_purple_circle: '🟣',
  white_circle: '⚪', black_circle: '⚫', o: '⭕', heavy_large_circle: '⭕',
  small_red_triangle: '🔺', small_red_triangle_down: '🔻', checkered_flag: '🏁',
  triangular_flag_on_post: '🚩', japan: '🗾', flag_jp: '🇯🇵', jp: '🇯🇵', flag_vn: '🇻🇳',
  vn: '🇻🇳', flag_us: '🇺🇸', us: '🇺🇸',
  // Nature & misc
  sunny: '☀️', cloud: '☁️', umbrella: '☔', snowflake: '❄️', rainbow: '🌈',
  earth_asia: '🌏', globe_with_meridians: '🌐', seedling: '🌱', herb: '🌿',
  four_leaf_clover: '🍀', cherry_blossom: '🌸', rose: '🌹', sunflower: '🌻',
  dog: '🐶', cat: '🐱', fox_face: '🦊', panda_face: '🐼', bear: '🐻', rabbit: '🐰',
  turtle: '🐢', snail: '🐌', octopus: '🐙', whale: '🐳', penguin: '🐧', chicken: '🐔',
  unicorn_face: '🦄', man: '👨', woman: '👩', person: '🧑', baby: '👶',
  man_technologist: '👨‍💻', woman_technologist: '👩‍💻', technologist: '🧑‍💻',
  ninja: '🥷', superhero: '🦸', running: '🏃', runner: '🏃', walking: '🚶',
  car: '🚗', airplane: '✈️', ship: '🚢', house: '🏠', office: '🏢', hospital: '🏥',
  money_with_wings: '💸', dollar: '💵', yen: '💴', credit_card: '💳', receipt: '🧾',
  shopping_trolley: '🛒', lipstick: '💄', ring: '💍', musical_note: '🎵', notes: '🎶',
  headphones: '🎧', microphone: '🎤', video_camera: '📹', movie_camera: '🎥', tv: '📺',
  game_die: '🎲', video_game: '🎮', soccer: '⚽', basketball: '🏀', tennis: '🎾',
  white_flag: '🏳️', black_flag: '🏴', recycle: '♻️', infinity: '♾️', copyright: '©️',
  registered: '®️', tm: '™️', hash: '#️⃣', one: '1️⃣', two: '2️⃣', three: '3️⃣',
  four: '4️⃣', five: '5️⃣', six: '6️⃣', seven: '7️⃣', eight: '8️⃣', nine: '9️⃣',
  zero: '0️⃣', keycap_ten: '🔟',
}

// Slack appends `::skin-tone-N:` (N = 2…6) to a shortcode; map it to the modifier.
const SKIN_TONES: Record<string, string> = {
  '2': '\u{1F3FB}',
  '3': '\u{1F3FC}',
  '4': '\u{1F3FD}',
  '5': '\u{1F3FE}',
  '6': '\u{1F3FF}',
}

/**
 * Unicode for a Slack emoji name (`+1`, `+1::skin-tone-3`, `pray`), or null
 * when unknown (custom emoji etc.).
 */
export function emojiFor(name: string): string | null {
  const [base, ...rest] = name.split('::')
  const glyph = EMOJI[base]
  if (!glyph) return null
  const tone = /^skin-tone-([2-6])$/.exec(rest[0] ?? '')
  // A modifier goes right after the base code point (before any variation selector).
  return tone ? glyph.replace(/️$/, '') + SKIN_TONES[tone[1]] : glyph
}
