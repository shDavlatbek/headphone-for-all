"""Unit tests of pick_ios_simulator.py: python3 -m unittest discover -s .github/scripts"""

import unittest

from pick_ios_simulator import pick, version_key


def runtime(version: str, platform: str = "iOS", available: bool = True) -> dict:
    ident = f"com.apple.CoreSimulator.SimRuntime.{platform}-{version.replace('.', '-')}"
    return {"identifier": ident, "version": version, "platform": platform, "isAvailable": available}


def iphone(udid: str, name: str = "iPhone 16") -> dict:
    return {"udid": udid, "name": name, "isAvailable": True}


class PickTest(unittest.TestCase):
    def test_version_key_is_numeric(self) -> None:
        self.assertLess(version_key("9.3"), version_key("18.5"))
        self.assertEqual(version_key("26.0.1"), (26, 0))
        self.assertEqual(version_key("26"), (26, 0))

    def test_skips_runtimes_newer_than_the_sdk(self) -> None:
        # macos-15 style: Xcode 16.4 (SDK 18.5) plus iOS 26 runtimes from a newer Xcode.
        runtimes = {"runtimes": [runtime("26.1"), runtime("18.5"), runtime("17.5")]}
        devices = {
            "devices": {
                runtimes["runtimes"][0]["identifier"]: [iphone("NEW")],
                runtimes["runtimes"][1]["identifier"]: [iphone("SDK")],
                runtimes["runtimes"][2]["identifier"]: [iphone("OLD")],
            }
        }
        self.assertEqual(pick("18.5", runtimes, devices), "SDK")

    def test_compares_versions_numerically_not_as_strings(self) -> None:
        # As strings "9.3" > "18.5"; numerically 18.5 is newer.
        runtimes = {"runtimes": [runtime("9.3"), runtime("18.5")]}
        devices = {
            "devices": {
                runtimes["runtimes"][0]["identifier"]: [iphone("NINE")],
                runtimes["runtimes"][1]["identifier"]: [iphone("EIGHTEEN")],
            }
        }
        self.assertEqual(pick("18.5", runtimes, devices), "EIGHTEEN")

    def test_patch_runtime_of_the_sdk_release_is_accepted(self) -> None:
        runtimes = {"runtimes": [runtime("26.0.1")]}
        devices = {"devices": {runtimes["runtimes"][0]["identifier"]: [iphone("PATCH")]}}
        self.assertEqual(pick("26.0", runtimes, devices), "PATCH")

    def test_ignores_other_platforms_ipads_and_unavailable(self) -> None:
        runtimes = {
            "runtimes": [
                runtime("18.5", platform="tvOS"),
                runtime("18.4", available=False),
                runtime("18.2"),
            ]
        }
        devices = {
            "devices": {
                runtimes["runtimes"][0]["identifier"]: [iphone("TV", name="Apple TV")],
                runtimes["runtimes"][1]["identifier"]: [iphone("GONE")],
                runtimes["runtimes"][2]["identifier"]: [
                    iphone("IPAD", name="iPad Pro 13-inch (M4)"),
                    iphone("PHONE", name="iPhone 16 Pro"),
                ],
            }
        }
        self.assertEqual(pick("18.5", runtimes, devices), "PHONE")

    def test_falls_back_to_an_older_runtime_with_an_iphone(self) -> None:
        runtimes = {"runtimes": [runtime("18.5"), runtime("18.0")]}
        devices = {
            "devices": {
                runtimes["runtimes"][0]["identifier"]: [iphone("IPAD", name="iPad Air")],
                runtimes["runtimes"][1]["identifier"]: [iphone("PHONE")],
            }
        }
        self.assertEqual(pick("18.5", runtimes, devices), "PHONE")

    def test_none_when_no_runtime_fits(self) -> None:
        runtimes = {"runtimes": [runtime("26.1")]}
        devices = {"devices": {runtimes["runtimes"][0]["identifier"]: [iphone("NEW")]}}
        self.assertIsNone(pick("18.5", runtimes, devices))

    def test_legacy_json_without_platform_key(self) -> None:
        rt = runtime("17.5")
        del rt["platform"]
        devices = {"devices": {rt["identifier"]: [iphone("LEGACY")]}}
        self.assertEqual(pick("17.5", {"runtimes": [rt]}, devices), "LEGACY")


if __name__ == "__main__":
    unittest.main()
