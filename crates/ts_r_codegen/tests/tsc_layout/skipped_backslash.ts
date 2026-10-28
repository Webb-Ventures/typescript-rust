// Comments next to a skipped invalid backslash are dropped, except one a line after it.
\ /*a*/ ;
/*b*/ \ /*c*/
\
/*kept*/ ;
