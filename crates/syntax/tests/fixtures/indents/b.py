def f(a, b):
    if a:
        return b
    elif b:
        pass
    else:
        for x in range(3):
            if x:
                break
            continue
        while (a and
               b):
            pass
    return [1,
            2]


class C:
    def m(self, x,
          y):
        x = foo(
            1, 2)
        return x
