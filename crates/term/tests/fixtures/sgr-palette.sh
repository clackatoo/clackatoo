#!/bin/sh
# Prints attributes, colours, wide characters, graphemes and line drawing:
# the source of sgr-palette.rec. See README.md for how to re-record it.
printf '\033[1mbold\033[0m \033[2mdim\033[0m \033[3mitalic\033[0m \033[4munder\033[0m '
printf '\033[4:3mcurly\033[0m \033[7minverse\033[0m \033[9mstrike\033[0m\r\n'
printf '\033[31mred \033[92mbright \033[38;5;208m208 \033[38;2;10;200;30mrgb \033[48;5;17;37mbg\033[0m\r\n'
printf '\033[41m\033[Kfull-width background\033[0m\r\n'
printf '\345\255\227 e\314\201 \360\237\221\251\342\200\215\360\237\222\273 |\r\n'
printf '\033(0lqqk\r\nx  x\r\nmqqj\033(B\r\n'
