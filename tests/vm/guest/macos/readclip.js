// Print the general pasteboard: change count, types, and the text (macOS).
//   osascript -l JavaScript readclip.js
ObjC.import('AppKit');
function run() {
  var pb = $.NSPasteboard.generalPasteboard;
  var out = ['changeCount ' + pb.changeCount];
  var types = pb.types;
  for (var i = 0; i < types.count; i++) out.push('type ' + ObjC.unwrap(types.objectAtIndex(i)));
  var s = pb.stringForType($.NSPasteboardTypeString);
  if (!s.isNil()) out.push('text ' + ObjC.unwrap(s));
  return out.join('\n');
}
