// Paste target: a window with a text view that saves its text to a file on
// every change (macOS, JavaScript for Automation). Pasted images are saved as
// FILE.image with their size, like the Linux and Windows editors.
//   osascript -l JavaScript editor.js [FILE]   (default /tmp/editor.txt)
ObjC.import('Cocoa');
function run(argv) {
  var path = argv[0] || '/tmp/editor.txt';
  var app = $.NSApplication.sharedApplication;
  app.setActivationPolicy($.NSApplicationActivationPolicyRegular);
  ObjC.registerSubclass({
    name: 'CMEditorDelegate', protocols: ['NSTextViewDelegate'],
    methods: {
      'textDidChange:': { types: ['void', ['id']], implementation: function (n) {
        var tv = n.object, text = tv.string;
        text.writeToFileAtomicallyEncodingError($(path), true, $.NSUTF8StringEncoding, null);
        var storage = tv.textStorage, att = storage.containsAttachments;
        if (att) {
          var size = '';
          storage.enumerateAttributeInRangeOptionsUsingBlock($.NSAttachmentAttributeName, $.NSMakeRange(0, storage.length), 0,
            function (value) { if (!value.isNil() && !value.image.isNil()) { var s = value.image.size; size = Math.round(s.width) + 'x' + Math.round(s.height); } });
          $(size).writeToFileAtomicallyEncodingError($(path + '.image'), true, $.NSUTF8StringEncoding, null);
        }
      } }
    }
  });
  var win = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer(
    $.NSMakeRect(300, 300, 640, 360),
    $.NSWindowStyleMaskTitled | $.NSWindowStyleMaskClosable | $.NSWindowStyleMaskResizable, $.NSBackingStoreBuffered, false);
  win.title = 'cm-editor';
  var tv = $.NSTextView.alloc.initWithFrame($.NSMakeRect(0, 0, 640, 360));
  tv.richText = true; tv.importsGraphics = true;
  tv.delegate = $.CMEditorDelegate.alloc.init;
  win.contentView = tv;
  win.makeKeyAndOrderFront(null);
  win.makeFirstResponder(tv);
  app.activateIgnoringOtherApps(true);
  app.run;
}
