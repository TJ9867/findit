#!/usr/bin/env python3
import argparse
import pathlib

def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument('file',nargs='+',type=pathlib.Path,help="file to clean")
    parser.add_argument('-x', "--x-out",nargs='+',type=str,help="Pattern to substitute with x's")

    return parser.parse_args()


def main():
    args = parse_args()
    for f in args.file:
        if f.exists():
            with open(f,"rb") as rf:
                data = rf.read()
                for x in args.x_out:
                    pat = x.encode('utf8')
                    print(f"Cleaning {pat}")
                    data = data.replace(pat, b"x" * len(pat))

            with open(f, "wb") as wf:
                wf.write(data)
        else:
            print(f"No such file {f}")
    print("Done.")

if __name__ == "__main__":
    main()
