import unittest

from inventory import ITEMS, total


class TestInventory(unittest.TestCase):
    def test_total(self):
        self.assertEqual(total(ITEMS), 55)


if __name__ == "__main__":
    unittest.main()
