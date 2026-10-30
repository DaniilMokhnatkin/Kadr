// NVAPI DRS ABI: https://github.com/NVIDIA/nvapi (nvapi.h, nvapi_interface.h).
// Overlay flags: NVIDIA Profile Inspector, nvidiaProfileInspector/CustomSettingNames.xml.
// Only Kadr's dedicated profile is writable. Never use the global/base profile APIs.
using System;
using System.IO;
using System.Runtime.InteropServices;

public static class KadrNvidiaProfile {
    const string Name = "Kadr screenshot utility";
    const uint Overlay = 0x809D5F60, DisableStatistics = 0x10000000;
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport("nvapi64.dll", CallingConvention = CallingConvention.Cdecl)]
    static extern IntPtr nvapi_QueryInterface(uint id);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int Init();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int CreateSession(out IntPtr session);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int SessionOp(IntPtr session);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl, CharSet = CharSet.Unicode)]
    delegate int FindApp(IntPtr session, string name, out IntPtr profile, ref Application app);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl, CharSet = CharSet.Unicode)]
    delegate int FindProfile(IntPtr session, string name, out IntPtr profile);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    delegate int CreateProfile(IntPtr session, ref Profile info, out IntPtr profile);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    delegate int GetProfile(IntPtr session, IntPtr profile, ref Profile info);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    delegate int CreateApp(IntPtr session, IntPtr profile, ref Application app);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    delegate int SetSetting(IntPtr session, IntPtr profile, ref Setting setting);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    delegate int GetSetting(IntPtr session, IntPtr profile, uint id, ref Setting setting);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct Profile {
        public uint version;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 2048)] public string name;
        public uint gpuSupport, predefined, apps, settings;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct Application {
        public uint version, predefined;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 2048)] public string name;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 2048)] public string friendly;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 2048)] public string launcher;
    }
    [StructLayout(LayoutKind.Sequential, Pack = 4, CharSet = CharSet.Unicode)]
    public struct Setting {
        public uint version;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 2048)] public string name;
        public uint id, type, location, predefined, predefinedValid;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 4100)] public byte[] defaultValue;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 4100)] public byte[] currentValue;
    }
    static uint Version(Type type) { return (uint)Marshal.SizeOf(type) | 0x10000; }
    static T Bind<T>(uint id) where T : class {
        var address = nvapi_QueryInterface(id);
        if (address == IntPtr.Zero) throw new InvalidOperationException("NVAPI function unavailable");
        return (T)(object)Marshal.GetDelegateForFunctionPointer(address, typeof(T));
    }
    static void Check(int status) {
        if (status != 0) throw new InvalidOperationException("NVAPI status " + status);
    }
    static Setting Value(uint id, uint value) {
        var setting = new Setting { version = Version(typeof(Setting)), id = id,
            name = "", defaultValue = new byte[4100], currentValue = new byte[4100] };
        Array.Copy(BitConverter.GetBytes(value), setting.currentValue, 4);
        return setting;
    }
    public static bool IsDedicated(Profile info) {
        return info.name == Name && info.predefined == 0 && info.apps <= 1;
    }
    // Read-only diagnostics also exercise the real driver ABI without creating/saving profiles.
    public static string Configure(bool apply) {
        if (IntPtr.Size != 8 || !File.Exists(Path.Combine(Environment.SystemDirectory, "nvapi64.dll")))
            return "NVIDIA not installed; skipped";
        Check(Bind<Init>(0x0150e828)());
        IntPtr session = IntPtr.Zero;
        try {
            Check(Bind<CreateSession>(0x0694d52e)(out session));
            Check(Bind<SessionOp>(0x375dbd6b)(session));
            var app = new Application { version = Version(typeof(Application)), name = "kadr.exe", friendly = "Kadr", launcher = "" };
            IntPtr profile;
            int status = Bind<FindApp>(0xeee566b2)(session, "kadr.exe", out profile, ref app);
            if (!apply) return status == 0 ? "NVIDIA available; Kadr profile found (read only)" : "NVIDIA available; Kadr profile not found (read only), status " + status;
            if (status == -166) {
                status = Bind<FindProfile>(0x7e4a9a0b)(session, Name, out profile);
                if (status == -163) {
                    var fresh = new Profile { version = Version(typeof(Profile)), name = Name };
                    Check(Bind<CreateProfile>(0xcc176068)(session, ref fresh, out profile));
                } else {
                    Check(status);
                    var old = new Profile { version = Version(typeof(Profile)), name = "" };
                    Check(Bind<GetProfile>(0x61cd6fd6)(session, profile, ref old));
                    if (!IsDedicated(old) || old.apps != 0) throw new InvalidOperationException("Existing profile is not exclusively Kadr; unchanged");
                }
                Check(Bind<CreateApp>(0x4347a9de)(session, profile, ref app));
            } else Check(status);
            var info = new Profile { version = Version(typeof(Profile)), name = "" };
            Check(Bind<GetProfile>(0x61cd6fd6)(session, profile, ref info));
            if (!IsDedicated(info) || info.apps != 1) throw new InvalidOperationException("Shared/foreign profile; unchanged");
            // Disable only Kadr's statistics/capture overlay and Ansel; games keep their settings.
            uint[] ids = { Overlay, 0x1075D972 };
            uint[] values = { DisableStatistics, 0 };
            for (int i = 0; i < ids.Length; i++) {
                var setting = Value(ids[i], values[i]);
                Check(Bind<SetSetting>(0x577dd202)(session, profile, ref setting));
            }
            Check(Bind<SessionOp>(0xfcbc7e14)(session));
            Check(Bind<SessionOp>(0x375dbd6b)(session));
            for (int i = 0; i < ids.Length; i++) {
                var setting = Value(ids[i], 0);
                Check(Bind<GetSetting>(0x73bf8338)(session, profile, ids[i], ref setting));
                if (setting.type != 0 || setting.location != 0 || BitConverter.ToUInt32(setting.currentValue, 0) != values[i])
                    throw new InvalidOperationException("NVIDIA profile verification failed");
            }
            return "NVIDIA overlay profile applied and verified for kadr.exe only";
        } finally {
            if (session != IntPtr.Zero) Bind<SessionOp>(0xdad9cff8)(session);
            Bind<Init>(0xd22bdd7e)();
        }
    }
}
