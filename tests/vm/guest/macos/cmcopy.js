// Put test data on the general pasteboard (macOS, JavaScript for Automation).
//   osascript -l JavaScript cmcopy.js text TEXT | big N | image WxH | file PATH... | secret TEXT | transient TEXT
// "secret" adds org.nspasteboard.ConcealedType (password-manager marker),
// "transient" adds org.nspasteboard.TransientType. Works on macOS 10.10+.
ObjC.import('AppKit');
function run(argv) {
  var kind = argv[0], rest = argv.slice(1);
  var pb = $.NSPasteboard.generalPasteboard;
  pb.clearContents;
  if (kind === 'text' || kind === 'secret' || kind === 'transient') {
    var types = [$.NSPasteboardTypeString];
    if (kind === 'secret') types.push('org.nspasteboard.ConcealedType');
    if (kind === 'transient') types.push('org.nspasteboard.TransientType');
    pb.declareTypesOwner($(types), $());
    pb.setStringForType($(rest.join(' ')), $.NSPasteboardTypeString);
    if (kind !== 'text') pb.setStringForType($(''), $(types[1]));
  } else if (kind === 'big') {
    var n = parseInt(rest[0], 10), s = '';
    while (s.length < n) s += 'clipmonger-big ';
    pb.setStringForType($(s.slice(0, n)), $.NSPasteboardTypeString);
  } else if (kind === 'image') {
    var wh = rest[0].split('x'), w = parseInt(wh[0], 10), h = parseInt(wh[1], 10);
    var rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(
      null, w, h, 8, 3, false, false, $.NSDeviceRGBColorSpace, w * 3, 24);
    for (var y = 0; y < h; y++) for (var x = 0; x < w; x++)
      rep.setColorAtXY($.NSColor.colorWithDeviceRedGreenBlueAlpha(x / w, y / h, 0.5, 1), x, y);
    var png = rep.representationUsingTypeProperties($.NSPNGFileType, $({})), tiff = rep.TIFFRepresentation;
    // The pixel loop takes seconds for larger images, and setDataForType
    // does not bump changeCount: a monitor polling changeCount would only
    // see the empty pasteboard cleared above. Declare the types (a new
    // change) once the data is ready.
    pb.declareTypesOwner($([$.NSPasteboardTypePNG, $.NSPasteboardTypeTIFF]), $());
    pb.setDataForType(png, $.NSPasteboardTypePNG);
    pb.setDataForType(tiff, $.NSPasteboardTypeTIFF);
  } else if (kind === 'file') {
    // Build the NSArray explicitly: $() of a JS array of ObjC objects
    // bridged only the first URL.
    var urls = $.NSMutableArray.array;
    rest.forEach(function (p) { urls.addObject($.NSURL.fileURLWithPath($(p))); });
    pb.writeObjects(urls);
  } else {
    throw new Error('usage: cmcopy.js text|big|image|file|secret|transient ...');
  }
  return 'changeCount ' + pb.changeCount;
}
