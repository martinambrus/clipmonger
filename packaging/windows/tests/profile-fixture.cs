// Private fixture executable. Root pre-creates this one empty owned directory.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
class ProfileFixture {
    const string Root = @"C:\Temp\clipmonger-profile-option-62523ba0-bd43-4a71-a8b5-e063347dcfc8";
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true, EntryPoint="CreateSymbolicLinkW", ExactSpelling=true)]
    [return: MarshalAs(UnmanagedType.U1)]
    static extern bool CreateSymbolicLink(string link, string target, int flags);
    const int AllowUnprivileged = 2;
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateFile(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool DeviceIoControl(SafeFileHandle handle, uint code, byte[] input, int inputSize, byte[] output, int outputSize, out int returned, IntPtr overlapped);
    static void Put16(byte[] b, int at, int value) { byte[] x = BitConverter.GetBytes(checked((ushort)value)); Buffer.BlockCopy(x, 0, b, at, 2); }
    static void Put32(byte[] b, int at, uint value) { byte[] x = BitConverter.GetBytes(value); Buffer.BlockCopy(x, 0, b, at, 4); }
    // Only called on fixed empty fixture directories and fixed sibling targets.
    static void CreateVerifiedJunction(string link, string target) {
        Directory.CreateDirectory(link);
        string substitute = @"\??\" + target;
        byte[] sub = Encoding.Unicode.GetBytes(substitute), print = Encoding.Unicode.GetBytes(target);
        byte[] data = new byte[16 + sub.Length + 2 + print.Length + 2];
        Put32(data, 0, 0xA0000003); Put16(data, 4, data.Length - 8);
        Put16(data, 8, 0); Put16(data, 10, sub.Length); Put16(data, 12, sub.Length + 2); Put16(data, 14, print.Length);
        Buffer.BlockCopy(sub, 0, data, 16, sub.Length); Buffer.BlockCopy(print, 0, data, 18 + sub.Length, print.Length);
        using (SafeFileHandle handle = CreateFile(link, 0x40000000, 3, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            int returned;
            if (!DeviceIoControl(handle, 0x000900A4, data, data.Length, null, 0, out returned, IntPtr.Zero)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            byte[] actual = new byte[16384];
            if (!DeviceIoControl(handle, 0x000900A8, null, 0, actual, actual.Length, out returned, IntPtr.Zero)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            Require(returned == data.Length, "junction data length changed");
            for (int i = 0; i < returned; ++i) Require(actual[i] == data[i], "junction tag or target changed");
            Require((File.GetAttributes(link) & FileAttributes.ReparsePoint) != 0, "junction attribute missing");
        }
    }
    static void Require(bool condition, string message) { if (!condition) throw new Exception(message); }
    static string Setup(string name) {
        string p = Path.Combine(Root, name); Directory.CreateDirectory(Path.Combine(p, "clipmonger"));
        File.WriteAllBytes(Path.Combine(p, "clipmonger", "data"), new byte[] { 100, 97, 116, 97 });
        File.WriteAllText(Path.Combine(p, "outside"), "sentinel");
        Directory.CreateDirectory(Path.Combine(p, "custom")); File.WriteAllText(Path.Combine(p, "custom", "data"), "custom");
        Directory.CreateDirectory(Path.Combine(p, "portable")); File.WriteAllText(Path.Combine(p, "portable", "data"), "portable");
        return p;
    }
    static void ProfileBytes(string p) {
        byte[] actual = File.ReadAllBytes(Path.Combine(p, "clipmonger", "data"));
        byte[] expected = new byte[] { 100, 97, 116, 97 };
        Require(actual.Length == expected.Length, "profile data length changed");
        for (int i = 0; i < expected.Length; ++i) Require(actual[i] == expected[i], "profile data bytes changed");
    }
    static void Intact(string p, bool target) {
        Require(File.ReadAllText(Path.Combine(p, "outside")) == "sentinel", "outside sentinel changed");
        Require(File.ReadAllText(Path.Combine(p, "custom", "data")) == "custom", "custom changed");
        Require(File.ReadAllText(Path.Combine(p, "portable", "data")) == "portable", "portable changed");
        Require(Directory.Exists(Path.Combine(p, "clipmonger")) == target, "unexpected profile state");
    }
    static void Refuse(Action a) { bool refused = false; try { a(); } catch { refused = true; } Require(refused, "expected refusal"); }
    static int Main(string[] args) {
        if (args.Length == 1 && args[0] == "--authority-probe") {
            bool available = ClipmongerProfileRemoval.TestAuthority();
            Console.WriteLine(available ? "AUTHORITY_OK" : "AUTHORITY_UNAVAILABLE");
            return available ? 0 : 2;
        }
        if (args.Length != 0) { Console.WriteLine("INVALID_MODE"); return 3; }
        using (ClipmongerProfileRemoval.PinFixture(Root)) {
            Require(Directory.GetFileSystemEntries(Root).Length == 0, "fixture must be empty");
            Console.WriteLine("STAGE unchecked");
            string p = Setup("unchecked"); ClipmongerProfileRemoval.TestDecision(p, false, false, "same", "same", false); Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE silent");
            p = Setup("silent"); ClipmongerProfileRemoval.TestDecision(p, true, true, "same", "same", false); Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE selected");
            p = Setup("selected"); ClipmongerProfileRemoval.TestDecision(p, true, false, "same", "same", false); Intact(p, false);
            Console.WriteLine("STAGE different");
            p = Setup("different"); Refuse(() => ClipmongerProfileRemoval.TestDecision(p, true, false, "other", "same", false)); Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE unknown");
            p = Setup("unknown"); Refuse(() => ClipmongerProfileRemoval.TestDecision(p, true, false, "", "same", false)); Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE changed");
            p = Setup("changed"); Refuse(() => ClipmongerProfileRemoval.TestDecision(p, true, false, "same", "same", true)); Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE known-false");
            p = Setup("known-false");
            bool createdOverExisting = CreateSymbolicLink(Path.Combine(p, "clipmonger", "data"), Path.Combine(p, "outside"), AllowUnprivileged);
            int knownError = Marshal.GetLastWin32Error();
            Require(!createdOverExisting, "existing file reported as link creation success");
            Require(knownError != 0, "known failure lacked Win32 error");
            Console.WriteLine("KNOWN_FALSE_WIN32 " + knownError);
            Intact(p, true); ProfileBytes(p);
            Console.WriteLine("STAGE nested-link");
            p = Setup("nested-link");
            CreateVerifiedJunction(Path.Combine(p, "clipmonger", "link"), Path.Combine(p, "custom"));
            Refuse(() => ClipmongerProfileRemoval.TestDecision(p, true, false, "same", "same", false)); Intact(p, true);
            Console.WriteLine("STAGE target-link");
            p = Path.Combine(Root, "target-link"); Directory.CreateDirectory(p);
            File.WriteAllText(Path.Combine(p, "outside"), "sentinel");
            Directory.CreateDirectory(Path.Combine(p, "custom")); File.WriteAllText(Path.Combine(p, "custom", "data"), "custom");
            Directory.CreateDirectory(Path.Combine(p, "portable")); File.WriteAllText(Path.Combine(p, "portable", "data"), "portable");
            CreateVerifiedJunction(Path.Combine(p, "clipmonger"), Path.Combine(p, "custom"));
            Refuse(() => ClipmongerProfileRemoval.TestDecision(p, true, false, "same", "same", false)); Intact(p, true);
            Console.WriteLine("STAGE race");
            p = Setup("race"); string race = p;
            bool hookRan = false, linkCreated = false; Exception hookFailure = null; string raceLink = null;
            ClipmongerProfileRemoval.BeforeOpen = entry => {
                hookRan = true;
                try {
                    raceLink = Path.Combine(race, "clipmonger", entry); File.Delete(raceLink);
                    CreateVerifiedJunction(raceLink, Path.Combine(race, "custom"));
                    linkCreated = true;
                    if (!linkCreated) throw new Exception("race entry is not a reparse point");
                } catch (Exception e) { hookFailure = e; throw; }
            };
            Refuse(() => ClipmongerProfileRemoval.TestDecision(race, true, false, "same", "same", false));
            if (hookFailure != null) throw new Exception("race setup failed", hookFailure);
            Require(hookRan && linkCreated && raceLink != null && (File.GetAttributes(raceLink) & FileAttributes.ReparsePoint) != 0, "race hook did not create a retained reparse point");
            Intact(p, true);
            Console.WriteLine("Fixture controls passed");
        }
        return 0;
    }
}
